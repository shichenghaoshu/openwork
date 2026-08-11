use openwork_e2e::{analyze, read_fixture_below, sha256_hex, validate_golden};
use std::fs;
use std::path::Path;

const JULY: &str = include_str!("../../../samples/sales/sales_july.csv");
const AUGUST: &str = include_str!("../../../samples/sales/sales_august.csv");
const ANALYSIS: &str = include_str!("../../../samples/sales/golden/sales-analysis.csv");
const SUMMARY: &str = include_str!("../../../samples/sales/golden/summary.md");

#[test]
fn checked_in_sales_fixture_matches_exact_golden() {
    let analysis = analyze(JULY, AUGUST).expect("analyze fixture");
    assert_eq!(analysis.july_total(), 33_000);
    assert_eq!(analysis.august_total(), 28_500);
    assert_eq!(analysis.change(), -4_500);
    assert_eq!(analysis.customers()[0].customer_name, "Crown");
    assert_eq!(analysis.customers()[0].decline, 3_000);
    validate_golden(&analysis, ANALYSIS, SUMMARY).expect("exact golden");
}

#[test]
fn hashes_pin_every_input_and_golden_file() {
    assert_eq!(
        sha256_hex(JULY.as_bytes()),
        "30ced8d2da54a3a0f1a7ce2f8043c50a8019466152d54f2f1be260176c86ecfc"
    );
    assert_eq!(
        sha256_hex(AUGUST.as_bytes()),
        "f4526344dafa85a6ef883b0ceef3e36717b3d3f3c88355f831e94583649f3678"
    );
    assert_eq!(
        sha256_hex(ANALYSIS.as_bytes()),
        "8d9c5ceb896dec30760bed5be362918fec36fbe96f090fd0cfed1d1ec5098a41"
    );
    assert_eq!(
        sha256_hex(SUMMARY.as_bytes()),
        "7b6b3a1c880790c523c9bd2f567340e31c41a4c893e869302929b38679432627"
    );
}

#[test]
fn duplicate_invalid_number_and_unstable_order_are_rejected() {
    let duplicate = JULY.replace("C002,Beta,5000\n", "C001,Beta,5000\n");
    assert!(analyze(&duplicate, AUGUST).is_err());
    let invalid_number = JULY.replace("8000", "80.00");
    assert!(analyze(&invalid_number, AUGUST).is_err());

    let analysis = analyze(JULY, AUGUST).expect("analysis");
    let unstable = ANALYSIS.replace(
        "C003,Crown,7000,4000,-3000,3000\nC001,Acme,8000,6000,-2000,2000\n",
        "C001,Acme,8000,6000,-2000,2000\nC003,Crown,7000,4000,-3000,3000\n",
    );
    assert!(validate_golden(&analysis, &unstable, SUMMARY).is_err());
}

#[test]
fn fixture_loader_rejects_traversal_and_symlinks() {
    let root = tempfile::tempdir().expect("root");
    fs::write(root.path().join("safe.txt"), "safe\n").expect("safe fixture");
    assert_eq!(
        read_fixture_below(root.path(), Path::new("safe.txt")).expect("read"),
        "safe\n"
    );
    assert!(read_fixture_below(root.path(), Path::new("../safe.txt")).is_err());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = tempfile::NamedTempFile::new().expect("outside");
        symlink(outside.path(), root.path().join("escape.txt")).expect("symlink");
        assert!(read_fixture_below(root.path(), Path::new("escape.txt")).is_err());
    }
}
