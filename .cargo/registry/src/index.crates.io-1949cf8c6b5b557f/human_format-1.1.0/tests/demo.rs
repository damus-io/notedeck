#[macro_use]
extern crate galvanic_test;
extern crate human_format;

test_suite! {
    name demo_examples;
    use human_format::*;

    test should_allow_use_of_si_scale_implicitly() {
        assert_eq!(Formatter::new()
            .format(1000 as f64),
            "1.00 K");
    }

    test should_allow_explicit_decimals() {
        assert_eq!(Formatter::new()
            .with_decimals(1)
            .format(1000 as f64),
            "1.0 K");
    }

    test should_allow_explicit_separator() {
        assert_eq!(Formatter::new()
            .with_separator(" - ")
            .format(1000 as f64),
            "1.00 - K");
    }

    test should_allow_use_of_si_scale_explicitly() {
        assert_eq!(Formatter::new()
            .with_scales(Scales::SI())
            .format(1000 as f64),
            "1.00 K");
    }

    test should_allow_use_of_binary_scale_explicitly() {
        assert_eq!(Formatter::new()
            .with_scales(Scales::Binary())
            .format(1024 as f64),
            "1.00 Ki");
    }

    test should_allow_use_of_binary_units_explicitly() {
        assert_eq!(Formatter::new()
            .with_scales(Scales::Binary())
            .with_units("B")
            .format(102400 as f64),
            "100.00 KiB");
    }

    test should_output_10_24_mib() {
        assert_eq!(Formatter::new()
            .with_scales(Scales::Binary())
            .with_units("B")
            .format(1024.0 * 1024.0 as f64),
            "1.00 MiB");
    }

    test should_output_75_11_pib() {
        assert_eq!(Formatter::new()
            .with_scales(Scales::Binary())
            .with_units("B")
            .format(84_567_942_345_572_238.0),
            "75.11 PiB");
    }

    test should_output_1_00_gbps() {
        assert_eq!(Formatter::new()
            .with_units("B/s")
            .format(1e9),
            "1.00 GB/s");
    }

    test should_allow_explicit_suffix_and_unit() {
        assert_eq!(Formatter::new()
            .with_suffix("k")
            .with_units("m")
            .format(1024 as f64),
            "1.02 Km");
    }

    test should_allow_use_of_explicit_scale() {
        let mut scales = Scales::new();

        scales
            .with_base(1024)
            .with_suffixes(vec!["","Ki", "Mi", "Gi", "Ti", "Pi", "Ei", "Zi", "Yi"]);

        assert_eq!(Formatter::new()
            .with_scales(scales)
            .with_units("B")
            .format(1024 as f64),
            "1.00 KiB");
    }

    test should_allow_parsing_to_f64() {
        assert_eq!(Formatter::new()
            .parse("1.00 K"), 1000.0);
    }

    test should_allow_try_parsing_to_f64() {
        assert_eq!(Formatter::new()
            .try_parse("1.00 M"), Ok(1000000.0));
    }

    test should_allow_parsing_binary_values_to_f64() {
        assert_eq!(Formatter::new()
            .with_scales(Scales::Binary())
            .parse("1.00 Ki"), 1024.0);
    }

    test should_allow_parsing_binary_values_with_units_to_f64() {
        assert_eq!(Formatter::new()
            .with_scales(Scales::Binary())
            .with_units("B")
            .parse("1.00 KiB"), 1024.0);
    }

    test should_allow_try_parsing_binary_values_with_units_to_f64() {
        assert_eq!(Formatter::new()
            .with_scales(Scales::Binary())
            .with_units("B")
            .try_parse("1.00 KiB"), Ok(1024.0));
    }

    test should_surface_errors() {
        let result = Formatter::new()
            .with_scales(Scales::Binary())
            .with_units("B")
            .try_parse("1.00 DN");

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Unknown suffix: DN, valid suffixes are: Ki, Mi, Gi, Ti, Pi, Ei, Zi, Yi");
    }

    test try_parse_explicit_suffix_and_unit() {
        assert_eq!(Formatter::new()
                   .with_units("m")
                   .try_parse("1.024Mm"), Ok(1024000.0));
    }

    test try_parse_explicit_suffix_and_unitless() {
        assert_eq!(Formatter::new()
                   .with_units("m")
                   .try_parse("1.024M"), Ok(1024000.0));
    }

    test try_parse_very_large_value() {
        assert_eq!(Formatter::new()
                   .with_units("B")
                   .try_parse("2PB"), Ok(2_000_000_000_000_000.0));
    }
}
