//! Cryptographically secure password generation.

use crate::crypto::Password;
use std::{
    fmt,
    ops::{BitOr, BitOrAssign},
};
use zeroize::Zeroizing;

const LOWER_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const UPPER_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGIT_CHARS: &[u8] = b"0123456789";
const SYMBOL_CHARS: &[u8] = b"!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
const RANDOM_BUFFER_LENGTH: usize = 128;

/// Maximum password length accepted by [`generate_password`].
pub const MAX_LENGTH: usize = 4096;

/// Character classes available to the password generator.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct CharClasses(u8);

impl CharClasses {
    /// No character classes selected.
    pub const EMPTY: Self = Self(0);
    /// Lowercase ASCII letters.
    pub const LOWER: Self = Self(0b0001);
    /// Uppercase ASCII letters.
    pub const UPPER: Self = Self(0b0010);
    /// ASCII decimal digits.
    pub const DIGITS: Self = Self(0b0100);
    /// Printable ASCII symbols excluding letters and digits.
    pub const SYMBOLS: Self = Self(0b1000);
    /// All supported character classes.
    pub const ALL: Self = Self(0b1111);

    /// Whether no character classes are selected.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether all flags in `other` are selected.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for CharClasses {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for CharClasses {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl fmt::Debug for CharClasses {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CharClasses(")?;
        let mut first = true;
        for (class, name) in [
            (Self::LOWER, "LOWER"),
            (Self::UPPER, "UPPER"),
            (Self::DIGITS, "DIGITS"),
            (Self::SYMBOLS, "SYMBOLS"),
        ] {
            if self.contains(class) {
                if !first {
                    formatter.write_str(" | ")?;
                }
                formatter.write_str(name)?;
                first = false;
            }
        }
        if first {
            formatter.write_str("EMPTY")?;
        }
        formatter.write_str(")")
    }
}

/// Errors returned by [`generate_password`].
#[derive(Debug)]
pub enum PasswordError {
    /// No character class was selected.
    NoCharacterClassSelected,
    /// A zero-length password was requested.
    ZeroLength,
    /// The requested length exceeds [`MAX_LENGTH`].
    LengthTooLarge { requested: usize, max: usize },
    /// The operating-system CSPRNG failed.
    Randomness(getrandom::Error),
}

impl fmt::Display for PasswordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCharacterClassSelected => {
                formatter.write_str("no password character class selected")
            }
            Self::ZeroLength => formatter.write_str("password length must be greater than zero"),
            Self::LengthTooLarge { requested, max } => {
                write!(
                    formatter,
                    "password length {requested} exceeds maximum {max}"
                )
            }
            Self::Randomness(error) => write!(formatter, "password randomness error: {error}"),
        }
    }
}

impl std::error::Error for PasswordError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Randomness(error) => Some(error),
            Self::NoCharacterClassSelected | Self::ZeroLength | Self::LengthTooLarge { .. } => None,
        }
    }
}

impl From<getrandom::Error> for PasswordError {
    fn from(error: getrandom::Error) -> Self {
        Self::Randomness(error)
    }
}

/// Generate a uniformly random password from the selected ASCII classes.
pub fn generate_password(length: usize, classes: CharClasses) -> Result<Password, PasswordError> {
    if classes.is_empty() {
        return Err(PasswordError::NoCharacterClassSelected);
    }
    if length == 0 {
        return Err(PasswordError::ZeroLength);
    }
    if length > MAX_LENGTH {
        return Err(PasswordError::LengthTooLarge {
            requested: length,
            max: MAX_LENGTH,
        });
    }

    let (charset, charset_length) = build_charset(classes);
    let limit = 256_u16 - (256_u16 % charset_length as u16);
    let mut random_bytes = Zeroizing::new([0_u8; RANDOM_BUFFER_LENGTH]);
    let mut cursor = RANDOM_BUFFER_LENGTH;
    let mut output = Zeroizing::new(Vec::with_capacity(length));

    while output.len() < length {
        if cursor == RANDOM_BUFFER_LENGTH {
            getrandom::fill(&mut *random_bytes)?;
            cursor = 0;
        }
        let byte = random_bytes[cursor];
        cursor += 1;
        if u16::from(byte) < limit {
            output.push(charset[usize::from(byte) % charset_length]);
        }
    }

    Ok(Password::new(&*output))
}

fn build_charset(classes: CharClasses) -> ([u8; 94], usize) {
    let mut charset = [0_u8; 94];
    let mut length = 0;
    for (class, chars) in [
        (CharClasses::LOWER, LOWER_CHARS),
        (CharClasses::UPPER, UPPER_CHARS),
        (CharClasses::DIGITS, DIGIT_CHARS),
        (CharClasses::SYMBOLS, SYMBOL_CHARS),
    ] {
        if classes.contains(class) {
            charset[length..length + chars.len()].copy_from_slice(chars);
            length += chars.len();
        }
    }
    (charset, length)
}

#[cfg(test)]
mod tests {
    use super::{CharClasses, MAX_LENGTH, PasswordError, build_charset, generate_password};

    const CLASSES: [CharClasses; 4] = [
        CharClasses::LOWER,
        CharClasses::UPPER,
        CharClasses::DIGITS,
        CharClasses::SYMBOLS,
    ];

    #[test]
    fn generated_passwords_honor_every_class_combination() {
        for mask in 1_u8..=0b1111 {
            let mut classes = CharClasses(0);
            for (index, class) in CLASSES.into_iter().enumerate() {
                if mask & (1 << index) != 0 {
                    classes |= class;
                }
            }
            let (charset, charset_length) = build_charset(classes);
            for length in [1, 2, 16, 64, MAX_LENGTH] {
                for _ in 0..32 {
                    let password = generate_password(length, classes).unwrap();
                    assert_eq!(password.len(), length);
                    assert!(
                        password
                            .as_bytes()
                            .iter()
                            .all(|byte| charset[..charset_length].contains(byte))
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_requests_are_rejected() {
        assert!(matches!(
            generate_password(1, CharClasses(0)),
            Err(PasswordError::NoCharacterClassSelected)
        ));
        assert!(matches!(
            generate_password(0, CharClasses::LOWER),
            Err(PasswordError::ZeroLength)
        ));
        let too_long = MAX_LENGTH + 1;
        assert!(matches!(
            generate_password(too_long, CharClasses::LOWER),
            Err(PasswordError::LengthTooLarge { requested, max })
                if requested == too_long && max == MAX_LENGTH
        ));
        assert_eq!(
            generate_password(MAX_LENGTH, CharClasses::LOWER)
                .unwrap()
                .len(),
            MAX_LENGTH
        );
    }

    #[test]
    fn class_flags_and_debug_output_are_useful() {
        let classes = CharClasses::LOWER | CharClasses::DIGITS;
        assert!(classes.contains(CharClasses::LOWER));
        assert!(classes.contains(CharClasses::DIGITS));
        assert!(!classes.contains(CharClasses::UPPER));
        assert_eq!(format!("{classes:?}"), "CharClasses(LOWER | DIGITS)");
        assert_eq!(format!("{:?}", CharClasses(0)), "CharClasses(EMPTY)");
        assert_eq!(
            CharClasses::ALL,
            classes | CharClasses::UPPER | CharClasses::SYMBOLS
        );
    }

    #[test]
    fn all_charset_is_exact_printable_ascii_without_space() {
        let (charset, length) = build_charset(CharClasses::ALL);
        assert_eq!(
            &charset[..length],
            b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"
        );
    }
}
