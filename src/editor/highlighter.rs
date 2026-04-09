use crate::editor::tokenizer::Token;

/// A token with a character position and length for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionedToken {
    pub token_type: String,
    pub pos: i64,
    pub len: i64,
}

/// DJB2 hash over positioned tokens for change detection.
pub fn calc_signature(tokens: &[PositionedToken]) -> i64 {
    if tokens.is_empty() {
        return 0;
    }
    let mut hash: i64 = 5381;
    for token in tokens {
        let part = format!("{}:{}:{}|", token.token_type, token.pos, token.len);
        for byte in part.bytes() {
            hash = ((hash.wrapping_mul(33)).wrapping_add(byte as i64)) % 2_147_483_647;
        }
    }
    hash
}

/// Convert flat token pairs to positioned tokens.
/// Each token's character length is computed using `char_counter`.
pub fn pair_tokens_to_positioned(tokens: &[Token]) -> Vec<PositionedToken> {
    let mut positioned = Vec::with_capacity(tokens.len());
    let mut pos: i64 = 0;
    for token in tokens {
        let text_len = token.text.chars().count() as i64;
        positioned.push(PositionedToken {
            token_type: token.token_type.clone(),
            pos,
            len: text_len,
        });
        pos += text_len;
    }
    positioned
}

/// Deep copy of a positioned token array.
pub fn clone_positioned(tokens: &[PositionedToken]) -> Vec<PositionedToken> {
    tokens.to_vec()
}

/// Merge adjacent positioned tokens of the same type.
pub fn merge_adjacent(tokens: &[PositionedToken]) -> Vec<PositionedToken> {
    let mut merged = Vec::new();
    for token in tokens {
        if token.len <= 0 {
            continue;
        }
        if let Some(prev) = merged.last_mut() {
            let prev: &mut PositionedToken = prev;
            if prev.token_type == token.token_type && prev.pos + prev.len == token.pos {
                prev.len += token.len;
                continue;
            }
        }
        merged.push(token.clone());
    }
    merged
}

/// Overlay semantic tokens on base tokens. Overlay tokens take precedence
/// where they cover the base token range.
pub fn overlay_positioned(
    base: &[PositionedToken],
    overlay: &[PositionedToken],
) -> Vec<PositionedToken> {
    if overlay.is_empty() {
        return clone_positioned(base);
    }

    let mut result = Vec::new();
    let mut overlay_idx = 0;

    for base_token in base {
        let base_end = base_token.pos + base_token.len;
        let mut cursor = base_token.pos;

        // Advance past overlay tokens ending before cursor
        while overlay_idx < overlay.len() {
            let ov = &overlay[overlay_idx];
            if ov.pos + ov.len <= cursor {
                overlay_idx += 1;
            } else {
                break;
            }
        }

        let mut scan_idx = overlay_idx;
        while cursor < base_end {
            if scan_idx >= overlay.len() {
                result.push(PositionedToken {
                    token_type: base_token.token_type.clone(),
                    pos: cursor,
                    len: base_end - cursor,
                });
                cursor = base_end;
                continue;
            }

            let ov = &overlay[scan_idx];

            if ov.pos >= base_end {
                result.push(PositionedToken {
                    token_type: base_token.token_type.clone(),
                    pos: cursor,
                    len: base_end - cursor,
                });
                cursor = base_end;
            } else if ov.pos > cursor {
                result.push(PositionedToken {
                    token_type: base_token.token_type.clone(),
                    pos: cursor,
                    len: ov.pos - cursor,
                });
                cursor = ov.pos;
            } else {
                let overlay_end = base_end.min(ov.pos + ov.len);
                if overlay_end > cursor {
                    result.push(PositionedToken {
                        token_type: ov.token_type.clone(),
                        pos: cursor,
                        len: overlay_end - cursor,
                    });
                    cursor = overlay_end;
                } else {
                    scan_idx += 1;
                }
            }
        }
    }

    merge_adjacent(&result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(tt: &str, text: &str) -> Token {
        Token {
            token_type: tt.into(),
            text: text.into(),
        }
    }

    fn ptok(tt: &str, pos: i64, len: i64) -> PositionedToken {
        PositionedToken {
            token_type: tt.into(),
            pos,
            len,
        }
    }

    #[test]
    fn pair_tokens_to_positioned_basic() {
        let tokens = vec![tok("keyword", "if"), tok("normal", " x")];
        let positioned = pair_tokens_to_positioned(&tokens);
        assert_eq!(
            positioned,
            vec![ptok("keyword", 0, 2), ptok("normal", 2, 2)]
        );
    }

    #[test]
    fn pair_tokens_to_positioned_multibyte() {
        let tokens = vec![tok("string", "\u{00E9}\u{00E8}")]; // 2 chars, 4 bytes
        let positioned = pair_tokens_to_positioned(&tokens);
        assert_eq!(positioned, vec![ptok("string", 0, 2)]);
    }

    #[test]
    fn calc_signature_deterministic() {
        let tokens = vec![ptok("keyword", 0, 2), ptok("normal", 2, 3)];
        let sig1 = calc_signature(&tokens);
        let sig2 = calc_signature(&tokens);
        assert_eq!(sig1, sig2);
        assert_ne!(sig1, 0);
    }

    #[test]
    fn calc_signature_empty() {
        assert_eq!(calc_signature(&[]), 0);
    }

    #[test]
    fn calc_signature_different_tokens() {
        let a = vec![ptok("keyword", 0, 2)];
        let b = vec![ptok("normal", 0, 2)];
        assert_ne!(calc_signature(&a), calc_signature(&b));
    }

    #[test]
    fn merge_adjacent_same_type() {
        let tokens = vec![ptok("normal", 0, 3), ptok("normal", 3, 2)];
        let merged = merge_adjacent(&tokens);
        assert_eq!(merged, vec![ptok("normal", 0, 5)]);
    }

    #[test]
    fn merge_adjacent_different_types() {
        let tokens = vec![ptok("keyword", 0, 2), ptok("normal", 2, 3)];
        let merged = merge_adjacent(&tokens);
        assert_eq!(merged, vec![ptok("keyword", 0, 2), ptok("normal", 2, 3)]);
    }

    #[test]
    fn merge_adjacent_skips_zero_length() {
        let tokens = vec![ptok("normal", 0, 0), ptok("keyword", 0, 3)];
        let merged = merge_adjacent(&tokens);
        assert_eq!(merged, vec![ptok("keyword", 0, 3)]);
    }

    #[test]
    fn overlay_empty_returns_base() {
        let base = vec![ptok("normal", 0, 10)];
        let result = overlay_positioned(&base, &[]);
        assert_eq!(result, base);
    }

    #[test]
    fn overlay_full_coverage() {
        let base = vec![ptok("normal", 0, 5)];
        let overlay = vec![ptok("keyword", 0, 5)];
        let result = overlay_positioned(&base, &overlay);
        assert_eq!(result, vec![ptok("keyword", 0, 5)]);
    }

    #[test]
    fn overlay_partial_split() {
        let base = vec![ptok("normal", 0, 10)];
        let overlay = vec![ptok("keyword", 3, 4)]; // covers pos 3..7
        let result = overlay_positioned(&base, &overlay);
        assert_eq!(
            result,
            vec![
                ptok("normal", 0, 3),
                ptok("keyword", 3, 4),
                ptok("normal", 7, 3),
            ]
        );
    }

    #[test]
    fn overlay_adjacent_merges() {
        let base = vec![ptok("normal", 0, 6)];
        let overlay = vec![ptok("normal", 2, 2)]; // same type as base
        let result = overlay_positioned(&base, &overlay);
        // After merge_adjacent, should be one span
        assert_eq!(result, vec![ptok("normal", 0, 6)]);
    }
}
