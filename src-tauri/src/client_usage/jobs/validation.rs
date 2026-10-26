//! Checks the raw Unicode and numeric validity which serde's IgnoredAny omits.
//! The retained state is bounded independently of message or number length.
#[derive(Default)]
pub(super) struct Validator {
    invalid: bool,
    utf8_left: u8,
    utf8_min: u8,
    utf8_max: u8,
    string: bool,
    escape: u8,
    unicode: u16,
    unicode_digits: u8,
    number: Option<Number>,
}
#[derive(Default)]
struct Number {
    token: Vec<u8>,
    long: bool,
    decimal: bool,
    exponent: bool,
    exponent_negative: bool,
    explicit_exponent: i64,
    integer_digits: i64,
    digits: i64,
    first_nonzero: Option<i64>,
    significant: Vec<u8>,
    extra_nonzero: bool,
}
impl Number {
    fn byte(&mut self, b: u8) {
        if self.token.len() < 1024 {
            self.token.push(b);
        } else {
            self.long = true;
        }
        if matches!(b, b'e' | b'E') {
            self.exponent = true;
            return;
        }
        if self.exponent {
            if b == b'-' {
                self.exponent_negative = true;
            } else if b.is_ascii_digit() {
                self.explicit_exponent = self
                    .explicit_exponent
                    .saturating_mul(10)
                    .saturating_add((b - b'0') as i64);
            }
            return;
        }
        if b == b'.' {
            self.decimal = true;
            return;
        }
        if !b.is_ascii_digit() {
            return;
        }
        if !self.decimal {
            self.integer_digits += 1;
        }
        if b != b'0' && self.first_nonzero.is_none() {
            self.first_nonzero = Some(self.digits);
        }
        self.digits += 1;
        if self.first_nonzero.is_some() {
            if self.significant.len() < 400 {
                self.significant.push(b);
            } else if b != b'0' {
                self.extra_nonzero = true;
            }
        }
    }
    fn valid(self) -> bool {
        if !self.long {
            return serde_json::from_slice::<serde_json::Value>(&self.token).is_ok();
        }
        let Some(first) = self.first_nonzero else {
            return true;
        };
        let exponent = if self.exponent_negative {
            self.explicit_exponent.saturating_neg()
        } else {
            self.explicit_exponent
        };
        let exponent = self
            .integer_digits
            .saturating_sub(first)
            .saturating_sub(1)
            .saturating_add(exponent);
        if exponent > 308 {
            return false;
        }
        if exponent < 308 {
            return true;
        }
        let mut normalized = String::new();
        normalized.push(self.significant[0] as char);
        normalized.push('.');
        for &b in &self.significant[1..] {
            normalized.push(b as char);
        }
        if self.extra_nonzero {
            normalized.push('1');
        }
        normalized.push_str("e308");
        serde_json::from_str::<serde_json::Value>(&normalized).is_ok()
    }
}
impl Validator {
    pub(super) fn feed(&mut self, bytes: &[u8]) {
        if self.invalid {
            return;
        }
        for &b in bytes {
            if self.utf8_left != 0 {
                if b < self.utf8_min || b > self.utf8_max {
                    self.invalid = true;
                    return;
                }
                self.utf8_left -= 1;
                self.utf8_min = 0x80;
                self.utf8_max = 0xbf;
            } else if b >= 0x80 {
                let (left, min, max) = match b {
                    0xc2..=0xdf => (1, 0x80, 0xbf),
                    0xe0 => (2, 0xa0, 0xbf),
                    0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf),
                    0xed => (2, 0x80, 0x9f),
                    0xf0 => (3, 0x90, 0xbf),
                    0xf1..=0xf3 => (3, 0x80, 0xbf),
                    0xf4 => (3, 0x80, 0x8f),
                    _ => {
                        self.invalid = true;
                        return;
                    }
                };
                self.utf8_left = left;
                self.utf8_min = min;
                self.utf8_max = max;
            }
            if self.string {
                match self.escape {
                    0 => {
                        if b == b'\\' {
                            self.escape = 1;
                        } else if b == b'"' {
                            self.string = false;
                        }
                    }
                    1 => {
                        if b == b'u' {
                            self.escape = 2;
                            self.unicode = 0;
                            self.unicode_digits = 0;
                        } else {
                            self.escape = 0;
                        }
                    }
                    2 | 5 => {
                        let n = match b {
                            b'0'..=b'9' => b - b'0',
                            b'a'..=b'f' => b - b'a' + 10,
                            b'A'..=b'F' => b - b'A' + 10,
                            _ => {
                                self.invalid = true;
                                return;
                            }
                        };
                        self.unicode = self.unicode * 16 + n as u16;
                        self.unicode_digits += 1;
                        if self.unicode_digits == 4 {
                            if self.escape == 5 {
                                if !(0xdc00..=0xdfff).contains(&self.unicode) {
                                    self.invalid = true;
                                    return;
                                }
                                self.escape = 0;
                            } else if (0xd800..=0xdbff).contains(&self.unicode) {
                                self.escape = 3;
                            } else if (0xdc00..=0xdfff).contains(&self.unicode) {
                                self.invalid = true;
                                return;
                            } else {
                                self.escape = 0;
                            }
                        }
                    }
                    3 => {
                        if b == b'\\' {
                            self.escape = 4;
                        } else {
                            self.invalid = true;
                            return;
                        }
                    }
                    4 => {
                        if b == b'u' {
                            self.escape = 5;
                            self.unicode = 0;
                            self.unicode_digits = 0;
                        } else {
                            self.invalid = true;
                            return;
                        }
                    }
                    _ => unreachable!(),
                }
            } else if let Some(number) = self.number.as_mut() {
                if matches!(b, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                    number.byte(b);
                } else {
                    if !self.number.take().unwrap().valid() {
                        self.invalid = true;
                        return;
                    }
                    if b == b'"' {
                        self.string = true;
                    }
                }
            } else if b == b'"' {
                self.string = true;
            } else if b == b'-' || b.is_ascii_digit() {
                let mut number = Number::default();
                number.byte(b);
                self.number = Some(number);
            }
        }
    }
    pub(super) fn valid(&mut self) -> bool {
        if let Some(number) = self.number.take() {
            if !number.valid() {
                self.invalid = true;
            }
        }
        !self.invalid && self.utf8_left == 0 && !self.string && self.escape == 0
    }
}
