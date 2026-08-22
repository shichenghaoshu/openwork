//! Deterministic fixtures and validators for the M1 safe-execution demo.

pub mod office_demo;
pub mod sales_demo;
pub mod scenario;

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::fs;
use std::path::{Component, Path};

const HEADER: &str = "customer_id,customer_name,sales,orders";
const MAX_SALES: i64 = 1_000_000_000_000;
const MAX_ORDERS: i64 = 1_000_000_000;

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
    orders: i64,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CustomerMetrics {
    july_orders: i64,
    august_orders: i64,
    growth_basis_points: i64,
    order_change: i64,
}

/// Complete exact analysis used to render both golden outputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SalesAnalysis {
    customers: Vec<CustomerAnalysis>,
    july_total: i64,
    august_total: i64,
    change: i64,
    decline: i64,
    july_orders: i64,
    august_orders: i64,
    order_change: i64,
    growth_basis_points: i64,
    order_growth_basis_points: i64,
    customer_metrics: Vec<CustomerMetrics>,
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
        let mut output = String::from(
            "customer_id,customer_name,july_sales,july_orders,august_sales,august_orders,sales_change,sales_decline,sales_growth_rate,order_change\n",
        );
        for (row, metrics) in self.customers.iter().zip(&self.customer_metrics) {
            writeln!(
                output,
                "{},{},{},{},{},{},{},{},{},{}",
                row.customer_id,
                row.customer_name,
                row.july_sales,
                metrics.july_orders,
                row.august_sales,
                metrics.august_orders,
                row.change,
                row.decline,
                format_rate(metrics.growth_basis_points),
                metrics.order_change
            )
            .expect("writing to a string cannot fail");
        }
        writeln!(
            output,
            "TOTAL,,{},{},{},{},{},{},{},{}",
            self.july_total,
            self.july_orders,
            self.august_total,
            self.august_orders,
            self.change,
            self.decline,
            format_rate(self.growth_basis_points),
            self.order_change
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
        let largest_metrics = self
            .customer_metrics
            .first()
            .ok_or(FixtureError("analysis metrics are incomplete"))?;
        Ok(format!(
            "# Sales comparison\n\n- July sales total: {}\n- August sales total: {}\n- Sales change: {} ({})\n- July order count: {}\n- August order count: {}\n- Order change: {} ({})\n- Largest sales decline: {} ({}), {} ({})\n- Acme sales decline: {}\n- Beta sales growth: {}\n- Delta sales change: {}\n",
            self.july_total,
            self.august_total,
            self.change,
            format_rate(self.growth_basis_points),
            self.july_orders,
            self.august_orders,
            self.order_change,
            format_rate(self.order_growth_basis_points),
            largest.customer_name,
            largest.customer_id,
            largest.decline,
            format_rate(largest_metrics.growth_basis_points),
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
    let mut metrics_by_customer = BTreeMap::new();
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
        let order_change = august_sale
            .orders
            .checked_sub(july_sale.orders)
            .ok_or(FixtureError("order arithmetic overflow"))?;
        metrics_by_customer.insert(
            customer_id.clone(),
            CustomerMetrics {
                july_orders: july_sale.orders,
                august_orders: august_sale.orders,
                growth_basis_points: growth_basis_points(change, july_sale.sales)?,
                order_change,
            },
        );
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
    let july_orders = checked_total(
        metrics_by_customer
            .values()
            .map(|metrics| metrics.july_orders),
    )?;
    let august_orders = checked_total(
        metrics_by_customer
            .values()
            .map(|metrics| metrics.august_orders),
    )?;
    let order_change = august_orders
        .checked_sub(july_orders)
        .ok_or(FixtureError("order arithmetic overflow"))?;
    let customer_metrics = customers
        .iter()
        .map(|customer| {
            metrics_by_customer
                .get(&customer.customer_id)
                .copied()
                .ok_or(FixtureError("analysis metrics are incomplete"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SalesAnalysis {
        customers,
        july_total,
        august_total,
        change,
        decline: change
            .checked_neg()
            .ok_or(FixtureError("sales arithmetic overflow"))?,
        july_orders,
        august_orders,
        order_change,
        growth_basis_points: growth_basis_points(change, july_total)?,
        order_growth_basis_points: growth_basis_points(order_change, july_orders)?,
        customer_metrics,
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
        if fields.len() != 4
            || !valid_customer_id(fields[0])
            || !valid_customer_name(fields[1])
            || !fields[2].bytes().all(|byte| byte.is_ascii_digit())
            || !fields[3].bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(FixtureError("sales CSV row is invalid"));
        }
        let amount = fields[2]
            .parse::<i64>()
            .map_err(|_| FixtureError("sales number is invalid"))?;
        let orders = fields[3]
            .parse::<i64>()
            .map_err(|_| FixtureError("order count is invalid"))?;
        if amount == 0
            || amount > MAX_SALES
            || orders == 0
            || orders > MAX_ORDERS
            || sales
                .insert(
                    fields[0].to_owned(),
                    MonthlySale {
                        customer_name: fields[1].to_owned(),
                        sales: amount,
                        orders,
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

fn growth_basis_points(change: i64, baseline: i64) -> Result<i64, FixtureError> {
    let magnitude = change
        .checked_abs()
        .ok_or(FixtureError("growth arithmetic overflow"))?;
    let scaled = magnitude
        .checked_mul(10_000)
        .and_then(|value| value.checked_add(baseline / 2))
        .ok_or(FixtureError("growth arithmetic overflow"))?
        / baseline;
    if change < 0 {
        scaled
            .checked_neg()
            .ok_or(FixtureError("growth arithmetic overflow"))
    } else {
        Ok(scaled)
    }
}

fn format_rate(basis_points: i64) -> String {
    let sign = if basis_points < 0 { "-" } else { "" };
    let magnitude = basis_points.unsigned_abs();
    format!("{sign}{}.{:02}%", magnitude / 100, magnitude % 100)
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
