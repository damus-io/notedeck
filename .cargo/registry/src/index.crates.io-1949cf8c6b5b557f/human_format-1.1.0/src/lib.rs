#![doc(html_root_url = "https://docs.rs/human_format")]

//! `human_format` provides facilitates creating a formatted string, converting between numbers that are beyond typical
//! needs for humans into a simpler string that conveys the gist of the meaning of the number.
//!
//! ## Setup
//!
//! Add the library to your dependencies listing
//!
//! ```toml
//! [dependencies]
//! human_format = "0.2"
//! ```
//!
//! Add the crate reference at your crate root
//!
//! ```rust
//! extern crate human_format;
//! ```
//!
//! Print some human readable strings
//!
//! ```rust
//! // "1.00 K"
//! let tmpStr = human_format::Formatter::new()
//!     .format(1000.0);
//! # assert_eq!(tmpStr, "1.00 K");
//!
//! // "1.00 M"
//! let tmpStr2 = human_format::Formatter::new()
//!     .format(1000000.0);
//! # assert_eq!(tmpStr2, "1.00 M");
//!
//! // "1.00 G"
//! let tmpStr3 = human_format::Formatter::new()
//!     .format(1000000000.0);
//! # assert_eq!(tmpStr3, "1.00 G");
//! ```
//!
//! If you are so inspired you can even try playing with units and customizing your `Scales`
//!
//! For more examples you should review the examples on github: [tests/demo.rs](https://github.com/BobGneu/human-format-rs/blob/master/tests/demo.rs)
//!

#[derive(Debug)]
struct ScaledValue {
    value: f64,
    suffix: String,
}

/// Entry point to the lib. Use this to handle your formatting needs.
#[derive(Debug)]
pub struct Formatter {
    decimals: usize,
    separator: String,
    scales: Scales,
    forced_units: String,
    forced_suffix: String,
}

impl Default for Formatter {
    fn default() -> Self {
        Formatter {
            decimals: 2,
            separator: " ".to_owned(),
            scales: Scales::SI(),
            forced_units: "".to_owned(),
            forced_suffix: "".to_owned(),
        }
    }
}

/// Provide a customized scaling scheme for your own modeling.
#[derive(Debug)]
pub struct Scales {
    base: u32,
    suffixes: Vec<String>,
}

impl Formatter {
    /// Initializes a new `Formatter` with default values.
    pub fn new() -> Self {
        Default::default()
    }

    /// Sets the decimals value for formatting the string.
    pub fn with_decimals(&mut self, decimals: usize) -> &mut Self {
        self.decimals = decimals;

        self
    }

    /// Sets the separator value for formatting the string.
    pub fn with_separator(&mut self, separator: &str) -> &mut Self {
        self.separator = separator.to_owned();

        self
    }

    /// Sets the scales value.
    pub fn with_scales(&mut self, scales: Scales) -> &mut Self {
        self.scales = scales;

        self
    }

    /// Sets the units value.
    pub fn with_units(&mut self, units: &str) -> &mut Self {
        self.forced_units = units.to_owned();

        self
    }

    /// Sets the expected suffix value.
    pub fn with_suffix(&mut self, suffix: &str) -> &mut Self {
        self.forced_suffix = suffix.to_owned();

        self
    }

    /// Formats the number into a string
    pub fn format(&self, value: f64) -> String {
        if value < 0.0 {
            return format!("-{}", self.format(value * -1.0));
        }

        let scaled_value = self.scales.to_scaled_value(value);

        format!(
            "{:.width$}{}{}{}",
            scaled_value.value,
            self.separator,
            scaled_value.suffix,
            self.forced_units,
            width = self.decimals
        )
    }

    /// Parse a string back into a float value.
    pub fn parse(&self, value: &str) -> f64 {
        let v: Vec<&str> = value.split(&self.separator).collect();

        let result = v.first().unwrap().parse::<f64>().unwrap();

        let mut suffix = v.get(1).unwrap().to_string();
        let new_len = suffix.len() - self.forced_units.len();

        suffix.truncate(new_len);

        let magnitude_multiplier = self.scales.get_magnitude_multiplier(&suffix);

        result * magnitude_multiplier
    }

    /// Attempt to parse a string back into a float value.
    pub fn try_parse(&self, value: &str) -> Result<f64, String> {
        // Remove suffix if present
        let value = value.trim_end_matches(&self.forced_units).to_string();

        // Find Suffix
        let mut number = String::new();
        for c in value.chars() {
            if c.is_digit(10) || c == '.' {
                number.push(c);
            } else {
                break;
            }
        }

        let suffix = value
            .trim_start_matches(&number)
            .trim_start_matches(&self.separator)
            .to_string();

        let number = number.parse::<f64>().map_err(|e| e.to_string())?;
        let magnitude_multiplier = self.scales.try_get_magnitude_multiplier(&suffix)?;

        Ok(number * magnitude_multiplier)
    }
}

impl Default for Scales {
    fn default() -> Self {
        Scales::SI()
    }
}

impl Scales {
    /// Instantiates a new `Scales` with SI keys
    pub fn new() -> Self {
        Scales::SI()
    }

    /// Instantiates a new `Scales` with SI keys
    #[allow(non_snake_case)]
    pub fn SI() -> Self {
        Scales {
            base: 1000,
            suffixes: vec![
                "".to_owned(),
                "K".to_owned(),
                "M".to_owned(),
                "G".to_owned(),
                "T".to_owned(),
                "P".to_owned(),
                "E".to_owned(),
                "Z".to_owned(),
                "Y".to_owned(),
            ],
        }
    }

    /// Instantiates a new `Scales` with Binary keys
    #[allow(non_snake_case)]
    pub fn Binary() -> Self {
        Scales {
            base: 1024,
            suffixes: vec![
                "".to_owned(),
                "Ki".to_owned(),
                "Mi".to_owned(),
                "Gi".to_owned(),
                "Ti".to_owned(),
                "Pi".to_owned(),
                "Ei".to_owned(),
                "Zi".to_owned(),
                "Yi".to_owned(),
            ],
        }
    }

    /// Sets the base for the `Scales`
    pub fn with_base(&mut self, base: u32) -> &mut Self {
        self.base = base;

        self
    }

    /// Sets the suffixes listing appropriately
    pub fn with_suffixes(&mut self, suffixes: Vec<&str>) -> &mut Self {
        self.suffixes = Vec::new();

        for suffix in suffixes {
            // This should be to_owned to be clear about intent.
            // https://users.rust-lang.org/t/to-string-vs-to-owned-for-string-literals/1441/6
            self.suffixes.push(suffix.to_owned());
        }

        self
    }

    fn try_get_magnitude_multiplier(&self, value: &str) -> Result<f64, String> {
        self.suffixes
            .iter()
            .enumerate()
            .find_map(|(idx, x)| {
                if value == x {
                    Some((self.base as f64).powi(idx as i32))
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                format!(
                    "Unknown suffix: {value}, valid suffixes are: {}",
                    self.suffixes
                        .iter()
                        .filter(|x| !x.trim().is_empty())
                        .map(String::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    fn get_magnitude_multiplier(&self, value: &str) -> f64 {
        for ndx in 0..self.suffixes.len() {
            if value == self.suffixes[ndx] {
                return (self.base as f64).powi(ndx as i32);
            }
        }

        0.0
    }

    fn to_scaled_value(&self, value: f64) -> ScaledValue {
        let mut index: usize = 0;
        let base: f64 = self.base as f64;
        let mut value = value;

        loop {
            if value < base {
                break;
            }

            value /= base;
            index += 1;
        }

        ScaledValue {
            value,
            suffix: self.suffixes[index].to_owned(),
        }
    }
}
