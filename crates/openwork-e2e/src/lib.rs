//! Deterministic fixtures and validators for the M1 safe-execution demo.

pub mod scenario;

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::fs;
use std::path::{Component, Path};

const HEADER: &str = "customer_id,customer_name,sales";
const MAX_SALES: i64 = 1_000_000_000_000;

/// Static, content-free fixture validation error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixtureError(&'static str);

impl fmt::Display for FixtureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for FixtureError {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MonthlySale {
    customer_name: String,
    sales: i64,
}

/// One deterministically ordered customer comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomerAnalysis {
    pub customer_id: String,
    pub customer_name: String,
    pub july_sales: i64,
    pub august_sales: i64,
    pub change: i64,
    pub decline: i64,
}

/// Complete exact analysis used to render both golden outputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SalesAnalysis {
    customers: Vec<CustomerAnalysis>,
    july_total: i64,
    august_total: i64,
    change: i64,
    decline: i64,
}

impl SalesAnalysis {
    #[must_use]
    pub fn customers(&self) -> &[CustomerAnalysis] {
        &self.customers
    }

    #[must_use]
    pub const fn july_total(&self) -> i64 {
        self.july_total
    }

    #[must_use]
    pub const fn august_total(&self) -> i64 {
        self.august_total
    }

    #[must_use]
    pub const fn change(&self) -> i64 {
        self.change
    }

    #[must_use]
    pub fn render_csv(&self) -> String {
        let mut output =
            String::from("customer_id,customer_name,july_sales,august_sales,change,decline\n");
        for row in &self.customers {
            writeln!(
                output,
                "{},{},{},{},{},{}",
                row.customer_id,
                row.customer_name,
                row.july_sales,
                row.august_sales,
                row.change,
                row.decline
            )
            .expect("writing to a string cannot fail");
        }
        writeln!(
            output,
            "TOTAL,,{},{},{},{}",
            self.july_total, self.august_total, self.change, self.decline
        )
        .expect("writing to a string cannot fail");
        output
    }

    /// Renders the fixed demo summary without locale-sensitive number formatting.
    ///
    /// # Errors
    ///
    /// Returns an error if an analysis not produced by [`analyze`] violates demo invariants.
    pub fn render_summary(&self) -> Result<String, FixtureError> {
        let largest = self
            .customers
            .first()
            .ok_or(FixtureError("analysis has no customers"))?;
        let acme = self.customer("Acme")?;
        let beta = self.customer("Beta")?;
        let delta = self.customer("Delta")?;
        Ok(format!(
            "# Sales comparison\n\n- July total: {}\n- August total: {}\n- Change: {}\n- Largest decline: {} ({}), {}\n- Acme decline: {}\n- Beta growth: {}\n- Delta change: {}\n",
            self.july_total,
            self.august_total,
            self.change,
            largest.customer_name,
            largest.customer_id,
            largest.decline,
            acme.decline,
            beta.change,
            delta.change
        ))
    }

    fn customer(&self, name: &str) -> Result<&CustomerAnalysis, FixtureError> {
        self.customers
            .iter()
            .find(|customer| customer.customer_name == name)
            .ok_or(FixtureError("required demo customer is missing"))
    }
}

/// Parses both fixed-format CSV inputs and returns stable decline ordering.
///
/// # Errors
///
/// Returns an error for invalid LF/CSV/numbers, duplicates, mismatched customers, or overflow.
pub fn analyze(july: &str, august: &str) -> Result<SalesAnalysis, FixtureError> {
    let july = parse_month(july)?;
    let august = parse_month(august)?;
    if july.len() != august.len() || july.keys().ne(august.keys()) {
        return Err(FixtureError("monthly customer sets differ"));
    }
    let mut customers = Vec::with_capacity(july.len());
    for (customer_id, july_sale) in july {
        let august_sale = august
            .get(&customer_id)
            .ok_or(FixtureError("monthly customer sets differ"))?;
        if july_sale.customer_name != august_sale.customer_name {
            return Err(FixtureError("customer names differ between months"));
        }
        let change = august_sale
            .sales
            .checked_sub(july_sale.sales)
            .ok_or(FixtureError("sales arithmetic overflow"))?;
        customers.push(CustomerAnalysis {
            customer_id,
            customer_name: july_sale.customer_name,
            july_sales: july_sale.sales,
            august_sales: august_sale.sales,
            change,
            decline: change
                .checked_neg()
                .ok_or(FixtureError("sales arithmetic overflow"))?,
        });
    }
    customers.sort_by(|left, right| {
        right
            .decline
            .cmp(&left.decline)
            .then_with(|| left.customer_id.cmp(&right.customer_id))
    });
    if ["Acme", "Beta", "Delta"].iter().any(|required| {
        !customers
            .iter()
            .any(|customer| &customer.customer_name == required)
    }) {
        return Err(FixtureError("required demo customer is missing"));
    }
    let july_total = checked_total(customers.iter().map(|row| row.july_sales))?;
    let august_total = checked_total(customers.iter().map(|row| row.august_sales))?;
    let change = august_total
        .checked_sub(july_total)
        .ok_or(FixtureError("sales arithmetic overflow"))?;
    Ok(SalesAnalysis {
        customers,
        july_total,
        august_total,
        change,
        decline: change
            .checked_neg()
            .ok_or(FixtureError("sales arithmetic overflow"))?,
    })
}

/// Requires byte-exact, LF-terminated golden output.
///
/// # Errors
///
/// Returns an error for content or ordering drift.
pub fn validate_golden(
    analysis: &SalesAnalysis,
    expected_csv: &str,
    expected_summary: &str,
) -> Result<(), FixtureError> {
    if analysis.render_csv() != expected_csv || analysis.render_summary()? != expected_summary {
        return Err(FixtureError("golden output mismatch"));
    }
    Ok(())
}

/// Reads one UTF-8 regular fixture without allowing absolute paths, traversal, or symlinks.
///
/// # Errors
///
/// Returns an error when the root/path/file is unsafe or content is not UTF-8.
pub fn read_fixture_below(root: &Path, relative: &Path) -> Result<String, FixtureError> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FixtureError("fixture path escapes its root"));
    }
    let root = fs::canonicalize(root).map_err(|_| FixtureError("fixture root unavailable"))?;
    let mut current = root.clone();
    for component in relative.components() {
        current.push(component);
        if fs::symlink_metadata(&current)
            .map_err(|_| FixtureError("fixture unavailable"))?
            .file_type()
            .is_symlink()
        {
            return Err(FixtureError("fixture symlinks are forbidden"));
        }
    }
    let target = fs::canonicalize(current).map_err(|_| FixtureError("fixture unavailable"))?;
    if !target.starts_with(&root) || !fs::metadata(&target).is_ok_and(|meta| meta.is_file()) {
        return Err(FixtureError("fixture path escapes its root"));
    }
    let bytes = fs::read(target).map_err(|_| FixtureError("fixture unavailable"))?;
    String::from_utf8(bytes).map_err(|_| FixtureError("fixture is not UTF-8"))
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn parse_month(input: &str) -> Result<BTreeMap<String, MonthlySale>, FixtureError> {
    if input.contains('\r') || !input.ends_with('\n') {
        return Err(FixtureError("sales CSV must be LF-terminated UTF-8"));
    }
    let mut lines = input.split_terminator('\n');
    if lines.next() != Some(HEADER) {
        return Err(FixtureError("sales CSV header is invalid"));
    }
    let mut sales = BTreeMap::new();
    for line in lines {
        let fields = line.split(',').collect::<Vec<_>>();
        if fields.len() != 3
            || !valid_customer_id(fields[0])
            || !valid_customer_name(fields[1])
            || !fields[2].bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(FixtureError("sales CSV row is invalid"));
        }
        let amount = fields[2]
            .parse::<i64>()
            .map_err(|_| FixtureError("sales number is invalid"))?;
        if amount > MAX_SALES
            || sales
                .insert(
                    fields[0].to_owned(),
                    MonthlySale {
                        customer_name: fields[1].to_owned(),
                        sales: amount,
                    },
                )
                .is_some()
        {
            return Err(FixtureError("sales amount or customer is invalid"));
        }
    }
    if sales.is_empty() {
        return Err(FixtureError("sales CSV has no customers"));
    }
    Ok(sales)
}

fn checked_total(mut values: impl Iterator<Item = i64>) -> Result<i64, FixtureError> {
    values.try_fold(0_i64, |total, value| {
        total
            .checked_add(value)
            .ok_or(FixtureError("sales arithmetic overflow"))
    })
}

fn valid_customer_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn valid_customer_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b' ' || byte == b'-')
}
