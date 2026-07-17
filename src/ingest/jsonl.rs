use serde::de::DeserializeOwned;

/// Return the largest prefix containing only complete JSONL records.
/// A valid final record is accepted even when it has no trailing newline.
pub(crate) fn safe_prefix_len<T: DeserializeOwned>(bytes: &[u8]) -> usize {
    let newline_safe_len = bytes
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |position| position + 1);
    if newline_safe_len < bytes.len()
        && serde_json::from_slice::<T>(&bytes[newline_safe_len..]).is_ok()
    {
        bytes.len()
    } else {
        newline_safe_len
    }
}

pub(crate) fn first_present(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}
