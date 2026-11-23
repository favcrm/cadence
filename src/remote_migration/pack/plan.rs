//! Upload planning, descriptor sealing, and streaming pack reader for remote
//! portable migration.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use sha2::{Digest, Sha256};

use super::super::{sha256_hex, PACK_BYTES, SEAL_KIND, SEAL_LAYOUT};
use super::{InventoryMember, Pack, PackPlan};
use crate::backup::portable::{Member, VerifiedPackage};
use crate::error::{Error, Result};
use crate::remote_cli::RemoteTarget;

/// packed-v1 plan over the sealed input inventory: manifest members plus
/// `manifest.json` itself, sorted by UTF-8 path bytes, then greedy whole-
/// member concatenation into ≤ 1 MiB packs with the manifest segment boundary
/// excluded — the exact deterministic plan the server replans and seals.
pub(in super::super) fn plan_input_packs(package: &VerifiedPackage) -> Result<PackPlan<'_>> {
    let mut inventory = Vec::with_capacity(package.manifest.members.len() + 1);
    inventory.extend(package.manifest.members.iter().map(
        |Member {
             path,
             bytes,
             sha256,
         }| InventoryMember {
            path: path.as_str(),
            sha256: sha256.as_str(),
            bytes: *bytes,
        },
    ));
    inventory.push(InventoryMember {
        path: "manifest.json",
        sha256: &package.manifest_sha256,
        bytes: package.manifest_bytes.len() as u64,
    });
    inventory.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    if inventory
        .windows(2)
        .any(|pair| pair[0].path == pair[1].path)
    {
        return Err(Error::rejected("package inventory has duplicate paths"));
    }
    let ordinal = inventory
        .iter()
        .position(|m| m.path == "manifest.json")
        .ok_or_else(|| Error::internal("sealed inventory lost the manifest"))?;
    let mut packs: Vec<Pack> = Vec::new();
    for (start, end) in [(0, ordinal), (ordinal + 1, inventory.len())] {
        if start == end {
            continue;
        }
        // Index of the pack currently collecting members; `None` closes it.
        let mut current: Option<usize> = None;
        for (at, member) in inventory.iter().enumerate().take(end).skip(start) {
            let streamed = member.bytes > PACK_BYTES;
            if current
                .map(|at| packs[at].bytes + member.bytes > PACK_BYTES)
                .unwrap_or(true)
            {
                packs.push(Pack {
                    index: packs.len(),
                    bytes: 0,
                    members: at..at,
                });
                current = Some(packs.len() - 1);
            }
            let pack = &mut packs[current.expect("pack just opened")];
            pack.members.end = at + 1;
            pack.bytes += member.bytes;
            if streamed {
                current = None;
            }
        }
    }
    Ok(PackPlan { inventory, packs })
}

/// The input-storage descriptor seal, byte-exact with the server's
/// `JSON.stringify` construction: fixed key order `{path,sha256,bytes}` per
/// member and `{manifestSha256,members}` for the inventory object.
pub(in super::super) fn seal_descriptor(
    target: &RemoteTarget,
    operation_id: &str,
    plan: &PackPlan<'_>,
) -> String {
    let manifest = &plan.inventory[plan
        .inventory
        .iter()
        .position(|m| m.path == "manifest.json")
        .unwrap_or(0)];
    let mut text = String::with_capacity(
        manifest.bytes as usize + target.org_id.len() + operation_id.len() + 256,
    );
    text.push_str("[\"");
    text.push_str(SEAL_KIND);
    text.push_str("\",\"");
    text.push_str(SEAL_LAYOUT);
    text.push_str("\",");
    js_string(&mut text, &target.org_id);
    text.push(',');
    js_string(&mut text, operation_id);
    text.push_str(",0,{\"manifestSha256\":");
    js_string(&mut text, manifest.sha256);
    text.push_str(",\"members\":[");
    for (at, member) in plan.inventory.iter().enumerate() {
        if at > 0 {
            text.push(',');
        }
        text.push_str("{\"path\":");
        js_string(&mut text, member.path);
        text.push_str(",\"sha256\":");
        js_string(&mut text, member.sha256);
        text.push_str(",\"bytes\":");
        text.push_str(&member.bytes.to_string());
        text.push('}');
    }
    text.push_str("]}]");
    sha256_hex(text.as_bytes())
}

/// Escape one JSON string exactly as `JSON.stringify` does. Unicode scalar
/// values, including U+2028/U+2029, remain literal UTF-8; only JSON's quotes,
/// backslashes and C0 controls require escaping.
pub(in super::super) fn js_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Streams one pack body: each member file is opened privately on first read
/// (never through a link, never a non-regular file), hashed as it flows, and
/// refused mid-stream — as an I/O error that aborts the request — when its
/// bytes or digest differ from the sealed inventory.
pub(in super::super) struct PackReader<'a, 'p> {
    source: &'a Path,
    members: &'a [InventoryMember<'p>],
    at: usize,
    file: Option<File>,
    remain: u64,
    hasher: Sha256,
}

impl<'a, 'p> PackReader<'a, 'p> {
    pub(in super::super) fn open(
        source: &'a Path,
        pack: &Pack,
        inventory: &'a [InventoryMember<'p>],
    ) -> Result<Self> {
        Ok(PackReader {
            source,
            members: &inventory[pack.members.clone()],
            at: 0,
            file: None,
            remain: 0,
            hasher: Sha256::new(),
        })
    }

    fn next_member(&mut self) -> io::Result<bool> {
        let Some(member) = self.members.get(self.at) else {
            return Ok(false);
        };
        let path = self.source.join(member.path);
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 || meta.size() != member.bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("package member {} changed during transfer", member.path),
            ));
        }
        self.file = Some(file);
        self.remain = member.bytes;
        self.hasher = Sha256::new();
        Ok(true)
    }

    fn finish_member(&mut self) -> io::Result<()> {
        let member = &self.members[self.at];
        if format!("{:x}", std::mem::take(&mut self.hasher).finalize()) != member.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "package member {} digest changed during transfer",
                    member.path
                ),
            ));
        }
        self.file = None;
        self.at += 1;
        Ok(())
    }
}

impl Read for PackReader<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.file.is_none() && !self.next_member()? {
                return Ok(0);
            }
            if self.remain == 0 {
                self.finish_member()?;
                continue;
            }
            let file = self.file.as_mut().expect("member open");
            let limit = buf.len().min(self.remain as usize);
            let read = file.read(&mut buf[..limit])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "package member {} truncated during transfer",
                        self.members[self.at].path
                    ),
                ));
            }
            self.hasher.update(&buf[..read]);
            self.remain -= read as u64;
            return Ok(read);
        }
    }
}
