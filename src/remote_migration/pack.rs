//! Pack planning, seal descriptor formatting, and streaming pack/part readers
//! for remote portable migration.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    common_binding, receipt_error, sha256_hex, strict_data, PACK_BYTES, SEAL_KIND, SEAL_LAYOUT,
};
use crate::backup::portable::{Member, VerifiedPackage};
use crate::error::{Error, Result};
use crate::remote_cli::RemoteTarget;

/// A sealed input member: the inventory row the server rebuilds. `path` is
/// the package-relative member path; `manifest.json` is added by the plan.
#[derive(Debug)]
pub(super) struct InventoryMember<'a> {
    pub(super) path: &'a str,
    pub(super) sha256: &'a str,
    pub(super) bytes: u64,
}

/// One packed-v1 upload unit: the members concatenated verbatim, in order.
#[derive(Debug)]
pub(super) struct Pack {
    /// `input-packs/<index>` path element.
    pub(super) index: usize,
    /// Exact bytes the PUT body must produce.
    pub(super) bytes: u64,
    /// Contiguous ordinal range in the shared sealed inventory.
    pub(super) members: std::ops::Range<usize>,
}

/// The deterministic packed-v1 input plan: sealed inventory plus packs.
pub(super) struct PackPlan<'a> {
    pub(super) inventory: Vec<InventoryMember<'a>>,
    pub(super) packs: Vec<Pack>,
}

/// Transport parts of one sealed pack: `max(1, ceil(bytes / bound))`, the
/// exact server arithmetic. `bound` is the published `packPartBytes`.
pub(super) fn part_count(bytes: u64, bound: u64) -> u64 {
    bytes.div_ceil(bound).max(1)
}

/// Byte count of 1-based `part`: `min(bound, total - (part-1)*bound)`. Callers
/// only pass `part` in `1..=part_count`, so the offset is below `total`.
pub(super) fn part_bytes_at(total: u64, bound: u64, part: u64) -> u64 {
    bound.min(total - (part - 1) * bound)
}

/// Length-limited view over one pack's stream: yields exactly `remain` bytes
/// of the underlying `PackReader`, which keeps its per-member hash state across
/// the part boundary. A member may straddle parts.
pub(super) struct PackPartReader<'r, 'a, 'p> {
    inner: &'r mut PackReader<'a, 'p>,
    remain: u64,
}

impl<'r, 'a, 'p> PackPartReader<'r, 'a, 'p> {
    pub(super) fn new(inner: &'r mut PackReader<'a, 'p>, bytes: u64) -> Self {
        PackPartReader {
            inner,
            remain: bytes,
        }
    }

    /// After the part is sent the stream must have yielded exactly its bytes;
    /// a short part means the pack ended early. Refuse, never retry.
    pub(super) fn finish_part(&self) -> Result<()> {
        if self.remain != 0 {
            return Err(Error::rejected(
                "package pack ended before its declared part bytes — nothing retried",
            ));
        }
        Ok(())
    }
}

impl Read for PackPartReader<'_, '_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remain == 0 || buf.is_empty() {
            return Ok(0);
        }
        let limit = buf.len().min(self.remain as usize);
        let read = self.inner.read(&mut buf[..limit])?;
        self.remain -= read as u64;
        Ok(read)
    }
}

/// Part receipt: the exact 11-key strict shape, bound to this operation, pack,
/// part and computed range. Any mismatch is a receipt refusal.
#[allow(clippy::too_many_arguments)]
pub(super) fn part_data(
    data: &Value,
    target: &RemoteTarget,
    operation_id: &str,
    pack: &Pack,
    descriptor: &str,
    part: u64,
    part_count: u64,
    range_bytes: u64,
) -> Result<()> {
    let object = strict_data(
        data,
        &[
            "format",
            "workspace",
            "operationId",
            "ok",
            "inputStorage",
            "descriptorSha256",
            "packIndex",
            "part",
            "partCount",
            "bytes",
            "generation",
        ],
    )?;
    common_binding(object, target)?;
    if object["ok"] != Value::Bool(true)
        || object["operationId"].as_str() != Some(operation_id)
        || object["inputStorage"].as_str() != Some(SEAL_LAYOUT)
        || object["descriptorSha256"].as_str() != Some(descriptor)
        || object["packIndex"].as_u64() != Some(pack.index as u64)
        || object["part"].as_u64() != Some(part)
        || object["partCount"].as_u64() != Some(part_count)
        || object["bytes"].as_u64() != Some(range_bytes)
        || object["generation"].as_u64() != Some(0)
    {
        return Err(receipt_error("pack part"));
    }
    Ok(())
}

/// After the last part: one more read on the underlying pack stream. This is
/// what verifies the trailing member's digest (the reader hashes at member
/// end), and it must return end-of-file; any byte means the pack is longer
/// than the plan and is refused.
pub(super) fn expect_pack_end(reader: &mut PackReader<'_, '_>) -> Result<()> {
    let mut probe = [0u8; 1];
    match reader.read(&mut probe) {
        Ok(0) => Ok(()),
        Ok(_) => Err(Error::rejected(
            "package pack is longer than its sealed plan — nothing retried",
        )),
        Err(_) => Err(Error::rejected(
            "package pack failed its sealed member check — nothing retried",
        )),
    }
}

/// Pack receipt: exact index, byte count, seal and per-member `(ordinal,
/// sha256, bytes)` rows — the same coverage the server verified.
pub(super) fn pack_data(
    data: &Value,
    target: &RemoteTarget,
    operation_id: &str,
    pack: &Pack,
    descriptor: &str,
    inventory: &[InventoryMember<'_>],
) -> Result<()> {
    let object = strict_data(
        data,
        &[
            "format",
            "workspace",
            "operationId",
            "ok",
            "inputStorage",
            "descriptorSha256",
            "packIndex",
            "bytes",
            "generation",
            "members",
        ],
    )?;
    common_binding(object, target)?;
    let rows = object["members"]
        .as_array()
        .filter(|rows| rows.len() == pack.members.len())
        .ok_or_else(|| receipt_error("pack"))?;
    let covered = pack.members.clone().zip(rows).all(|(ordinal, row)| {
        let member = &inventory[ordinal];
        row.as_array().is_some_and(|r| {
            r.len() == 3
                && r[0].as_u64() == Some(ordinal as u64)
                && r[1].as_str() == Some(member.sha256)
                && r[2].as_u64() == Some(member.bytes)
        })
    });
    if object["ok"] != Value::Bool(true)
        || object["operationId"].as_str() != Some(operation_id)
        || object["inputStorage"].as_str() != Some(SEAL_LAYOUT)
        || object["descriptorSha256"].as_str() != Some(descriptor)
        || object["packIndex"].as_u64() != Some(pack.index as u64)
        || object["bytes"].as_u64() != Some(pack.bytes)
        || object["generation"].as_u64() != Some(0)
        || !covered
    {
        return Err(receipt_error("pack"));
    }
    Ok(())
}

/// packed-v1 plan over the sealed input inventory: manifest members plus
/// `manifest.json` itself, sorted by UTF-8 path bytes, then greedy whole-
/// member concatenation into ≤ 1 MiB packs with the manifest segment boundary
/// excluded — the exact deterministic plan the server replans and seals.
pub(super) fn plan_input_packs(package: &VerifiedPackage) -> Result<PackPlan<'_>> {
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
pub(super) fn seal_descriptor(
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
pub(super) fn js_string(out: &mut String, value: &str) {
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
pub(super) struct PackReader<'a, 'p> {
    source: &'a Path,
    members: &'a [InventoryMember<'p>],
    at: usize,
    file: Option<File>,
    remain: u64,
    hasher: Sha256,
}

impl<'a, 'p> PackReader<'a, 'p> {
    pub(super) fn open(
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
