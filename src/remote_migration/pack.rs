//! Pack planning, seal descriptor formatting, and streaming pack/part readers
//! for remote portable migration.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

#[path = "pack/plan.rs"]
mod plan;

use std::io::{self, Read};

use serde_json::Value;

use super::{common_binding, receipt_error, strict_data, SEAL_LAYOUT};
use crate::error::{Error, Result};
use crate::remote_cli::RemoteTarget;

#[cfg(test)]
pub(super) use plan::js_string;
pub(super) use plan::{plan_input_packs, seal_descriptor, PackReader};

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
