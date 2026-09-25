// SPDX-License-Identifier: Apache-2.0
//! Bounds-checked little-endian reads for the component parsers.
//!
//! Every archive and vector component is read from an untrusted store. A
//! declared length is an instruction from that store, so each read either
//! lands inside the bytes or answers `None`, which the parser reports as an
//! integrity error. The end of a range is `checked_add`ed: a length near
//! `u64::MAX` must read as "past the end", never wrap into a plausible range
//! or reach a slice index that panics (P15/R32).

/// `len` bytes at `pos`, or `None` when that range is not inside `bytes`.
pub(crate) fn slice_at(bytes: &[u8], pos: usize, len: usize) -> Option<&[u8]> {
    bytes.get(pos..pos.checked_add(len)?)
}

/// The `N` bytes at `pos` as an array, or `None` past the end.
pub(crate) fn array_at<const N: usize>(bytes: &[u8], pos: usize) -> Option<[u8; N]> {
    slice_at(bytes, pos, N)?.try_into().ok()
}

pub(crate) fn le_u32_at(bytes: &[u8], pos: usize) -> Option<u32> {
    array_at(bytes, pos).map(u32::from_le_bytes)
}

pub(crate) fn le_u64_at(bytes: &[u8], pos: usize) -> Option<u64> {
    array_at(bytes, pos).map(u64::from_le_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_land_inside_the_bytes_or_answer_none() {
        let bytes = [1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(le_u32_at(&bytes, 0), Some(1));
        assert_eq!(le_u64_at(&bytes, 4), Some(2));
        assert_eq!(le_u64_at(&bytes, 5), None, "one byte short");
        assert_eq!(slice_at(&bytes, 12, 0), Some(&[][..]), "empty at the end");
        assert_eq!(slice_at(&bytes, 13, 0), None, "start past the end");
        assert_eq!(slice_at(&bytes, 4, usize::MAX), None, "end overflows");
        assert_eq!(array_at::<4>(&bytes, usize::MAX), None);
    }
}
