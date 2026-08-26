//! ERP adapter boundary and the deterministic mock backend.
//!
//! A real ERP (SAP, Odoo, Kingdee, Yonyou) plugs in by implementing
//! [`ErpAdapter`]; policy and approval semantics are unchanged. The mock
//! backend is deterministic and local, suitable for demos and E2E: budget and
//! vendor data are constants, purchase request IDs are sequential from
//! `PR-2026-0001`, and writes happen only through
//! [`ErpAdapter::create_purchase_request`] after an approval claim.

use openwork_execution::UtcTimestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// Constants for the deterministic mock ERP.
const MOCK_YEAR: u32 = 2026;
const MOCK_TOTAL_BUDGET: f64 = 500_000.0;

/// Reads one remaining budget slice. Read-only and never writes state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BudgetInfo {
    pub department: String,
    pub currency: String,
    pub total_budget: f64,
    pub committed: f64,
    pub remaining: f64,
    pub period: String,
}

/// One approved vendor read. Read-only.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Vendor {
    pub name: String,
    pub category: String,
    pub rating: String,
}

/// One catalog item read. Read-only.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CatalogItem {
    pub sku: String,
    pub name: String,
    pub unit_price: f64,
    pub currency: String,
}

/// Exact structured purchase request produced by the governed flow.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PurchaseRequest {
    pub requester: String,
    pub department: String,
    pub cost_center: String,
    pub item_description: String,
    pub quantity: u64,
    pub unit_estimate: f64,
    pub currency: String,
    pub vendor: Option<String>,
    pub justification: String,
    pub delivery_location: String,
    pub requested_date: String,
    pub budget_reference: Option<String>,
}

impl PurchaseRequest {
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn estimated_total(&self) -> f64 {
        self.unit_estimate * self.quantity as f64
    }
}

/// Stable external result of a mock ERP write.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PurchaseRequestReceipt {
    pub erp_request_id: String,
    pub status: String,
    pub estimated_total: f64,
    pub currency: String,
    pub created_at: String,
}

/// One deterministic mock ERP error; messages never leak into audit logs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErpError {
    InvalidRequest,
    BudgetInsufficient,
    UnknownVendor,
    WriteFailed,
}

/// The ERP adapter boundary. Policy, approval, and credential semantics live
/// outside this trait, so swapping the backend never changes governance.
pub trait ErpAdapter: Send + Sync {
    /// Reads the remaining budget for one department.
    ///
    /// # Errors
    ///
    /// Returns [`ErpError::InvalidRequest`] for an unknown or malformed department.
    fn budget(&self, department: &str) -> Result<BudgetInfo, ErpError>;
    /// Searches approved vendors.
    ///
    /// # Errors
    ///
    /// Returns [`ErpError::InvalidRequest`] for a malformed query.
    fn vendors(&self, query: &str) -> Result<Vec<Vendor>, ErpError>;
    /// Searches the catalog for items.
    ///
    /// # Errors
    ///
    /// Returns [`ErpError::InvalidRequest`] for a malformed query.
    fn items(&self, query: &str) -> Result<Vec<CatalogItem>, ErpError>;
    /// Creates one purchase request. Callers must hold a consumed approval
    /// claim; the adapter never writes on a denied or expired approval.
    ///
    /// # Errors
    ///
    /// Returns an [`ErpError`] for an invalid request, an unapproved vendor,
    /// or a request that exceeds the department budget.
    fn create_purchase_request(
        &self,
        request: &PurchaseRequest,
    ) -> Result<PurchaseRequestReceipt, ErpError>;
}

/// Deterministic, process-local mock ERP backend.
#[derive(Debug)]
pub struct MockErpAdapter {
    /// Sequential request counter; every process instance starts at 1 so demos
    /// reproduce `PR-2026-0001`.
    created_requests: AtomicU64,
}

impl MockErpAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            created_requests: AtomicU64::new(0),
        }
    }
}

impl Default for MockErpAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ErpAdapter for MockErpAdapter {
    fn budget(&self, department: &str) -> Result<BudgetInfo, ErpError> {
        if department.is_empty() || department.len() > 128 {
            return Err(ErpError::InvalidRequest);
        }
        // The mock backend knows the Shanghai office, headquarters, and sales
        // departments with realistic committed amounts.
        let (committed, period) = match department {
            "shanghai-office" => (210_000.0, "FY2026-Q3"),
            "headquarters" => (320_000.0, "FY2026-Q3"),
            "sales" => (95_000.0, "FY2026-Q3"),
            _ => return Err(ErpError::InvalidRequest),
        };
        Ok(BudgetInfo {
            department: department.to_owned(),
            currency: "CNY".to_owned(),
            total_budget: MOCK_TOTAL_BUDGET,
            committed,
            remaining: MOCK_TOTAL_BUDGET - committed,
            period: period.to_owned(),
        })
    }

    fn vendors(&self, query: &str) -> Result<Vec<Vendor>, ErpError> {
        if query.len() > 128 {
            return Err(ErpError::InvalidRequest);
        }
        let mut matches = Vec::new();
        for vendor in mock_vendors() {
            if query.is_empty() || vendor.name.contains(query) || vendor.category.contains(query) {
                matches.push(Vendor {
                    name: vendor.name.to_owned(),
                    category: vendor.category.to_owned(),
                    rating: vendor.rating.to_owned(),
                });
            }
        }
        Ok(matches)
    }

    fn items(&self, query: &str) -> Result<Vec<CatalogItem>, ErpError> {
        if query.len() > 128 {
            return Err(ErpError::InvalidRequest);
        }
        let mut matches = Vec::new();
        for item in mock_items() {
            let lower = query.to_ascii_lowercase();
            if query.is_empty()
                || item.name.contains(query)
                || item.sku.to_ascii_lowercase().contains(&lower)
            {
                matches.push(CatalogItem {
                    sku: item.sku.to_owned(),
                    name: item.name.to_owned(),
                    unit_price: item.unit_price,
                    currency: item.currency.to_owned(),
                });
            }
        }
        Ok(matches)
    }

    fn create_purchase_request(
        &self,
        request: &PurchaseRequest,
    ) -> Result<PurchaseRequestReceipt, ErpError> {
        if request.requester.trim().is_empty()
            || request.department.trim().is_empty()
            || request.item_description.trim().is_empty()
            || request.quantity == 0
            || request.quantity > 100_000
            || !request.unit_estimate.is_finite()
            || request.unit_estimate <= 0.0
            || request.unit_estimate > 10_000_000.0
            || request.justification.trim().is_empty()
            || request.delivery_location.trim().is_empty()
        {
            return Err(ErpError::InvalidRequest);
        }
        if request
            .budget_reference
            .as_deref()
            .is_some_and(|reference| reference.trim().is_empty())
        {
            return Err(ErpError::InvalidRequest);
        }
        let vendors = self.vendors("")?;
        if request
            .vendor
            .as_deref()
            .is_some_and(|vendor| !vendors.iter().any(|candidate| candidate.name == vendor))
        {
            return Err(ErpError::UnknownVendor);
        }
        let budget = self.budget(&request.department)?;
        if request.estimated_total() > budget.remaining {
            return Err(ErpError::BudgetInsufficient);
        }
        let sequence = self.created_requests.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(PurchaseRequestReceipt {
            erp_request_id: format!("PR-{MOCK_YEAR}-{sequence:04}"),
            status: "created".to_owned(),
            estimated_total: request.estimated_total(),
            currency: request.currency.clone(),
            created_at: UtcTimestamp::now().to_rfc3339_string(),
        })
    }
}

struct MockVendor {
    name: &'static str,
    category: &'static str,
    rating: &'static str,
}

const fn mock_vendors() -> &'static [MockVendor] {
    &[
        MockVendor {
            name: "Shanghai Display Supply",
            category: "displays",
            rating: "A",
        },
        MockVendor {
            name: "Jingdong Enterprise",
            category: "electronics",
            rating: "A",
        },
        MockVendor {
            name: "Lenovo Channel Partner",
            category: "computers",
            rating: "B",
        },
    ]
}

struct MockItem {
    sku: &'static str,
    name: &'static str,
    unit_price: f64,
    currency: &'static str,
}

const fn mock_items() -> &'static [MockItem] {
    &[
        MockItem {
            sku: "MON-27-IPS",
            name: "27-inch IPS office monitor",
            unit_price: 1_850.0,
            currency: "CNY",
        },
        MockItem {
            sku: "LAP-14-W11",
            name: "14-inch lightweight laptop",
            unit_price: 6_300.0,
            currency: "CNY",
        },
        MockItem {
            sku: "DOCK-USB-C",
            name: "USB-C dock station",
            unit_price: 780.0,
            currency: "CNY",
        },
    ]
}

/// Serializes a purchase request for artifact generation; values are safe for
/// display and never contain secrets.
///
/// # Panics
///
/// Panics only if a purchase request ever fails to serialize to JSON.
#[must_use]
pub fn purchase_request_form(request: &PurchaseRequest) -> Value {
    serde_json::to_value(request).expect("purchase request is always JSON-serializable")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> PurchaseRequest {
        PurchaseRequest {
            requester: "li.wei@example.invalid".to_owned(),
            department: "shanghai-office".to_owned(),
            cost_center: "SH-1001".to_owned(),
            item_description: "27-inch IPS office monitor".to_owned(),
            quantity: 20,
            unit_estimate: 1_850.0,
            currency: "CNY".to_owned(),
            vendor: Some("Shanghai Display Supply".to_owned()),
            justification: "Bring the Shanghai office to dual-monitor workspaces.".to_owned(),
            delivery_location: "Shanghai, Zhangjiang Rd 88".to_owned(),
            requested_date: "2026-09-15".to_owned(),
            budget_reference: Some("BUDGET-FY2026-SH-OFFICE".to_owned()),
        }
    }

    #[test]
    fn reads_are_read_only_and_deterministic() {
        let erp = MockErpAdapter::new();
        let budget = erp.budget("shanghai-office").expect("budget");
        assert!((budget.remaining - (MOCK_TOTAL_BUDGET - 210_000.0)).abs() < 0.01);
        assert_eq!(budget.period, "FY2026-Q3");
        assert!(budget.remaining > request().estimated_total());
        let vendors = erp.vendors("Display").expect("vendors");
        assert_eq!(vendors.len(), 1);
        assert_eq!(vendors[0].name, "Shanghai Display Supply");
        let items = erp.items("monitor").expect("items");
        assert_eq!(items[0].sku, "MON-27-IPS");
        assert_eq!(items.len(), 1);
        // Unknown department fails closed.
        assert_eq!(erp.budget("unknown"), Err(ErpError::InvalidRequest));
    }

    #[test]
    fn purchase_requests_are_sequential_and_stable() {
        let erp = MockErpAdapter::new();
        let first = erp
            .create_purchase_request(&request())
            .expect("first request");
        assert_eq!(first.erp_request_id, "PR-2026-0001");
        assert_eq!(first.status, "created");
        assert!((first.estimated_total - 37_000.0).abs() < 0.01);
        assert_eq!(first.currency, "CNY");
        let second = erp
            .create_purchase_request(&request())
            .expect("second request");
        assert_eq!(second.erp_request_id, "PR-2026-0002");
    }

    #[test]
    fn invalid_or_unauthorized_requests_never_write() {
        let erp = MockErpAdapter::new();
        let mut invalid = request();
        invalid.quantity = 0;
        assert_eq!(
            erp.create_purchase_request(&invalid),
            Err(ErpError::InvalidRequest)
        );
        let mut over = request();
        over.unit_estimate = 30_000.0;
        assert_eq!(
            erp.create_purchase_request(&over),
            Err(ErpError::BudgetInsufficient)
        );
        let mut unknown_vendor = request();
        unknown_vendor.vendor = Some("Not Approved Corp".to_owned());
        assert_eq!(
            erp.create_purchase_request(&unknown_vendor),
            Err(ErpError::UnknownVendor)
        );
        // A denied approval never reaches the adapter; the count stays at zero
        // and the next successful write is still PR-2026-0001.
        assert_eq!(erp.created_requests.load(Ordering::SeqCst), 0);
        let first = erp.create_purchase_request(&request());
        assert_eq!(first.expect("success").erp_request_id, "PR-2026-0001");
    }

    #[test]
    fn purchase_request_form_contains_all_demanded_fields() {
        let form = purchase_request_form(&request());
        for field in [
            "requester",
            "department",
            "cost_center",
            "item_description",
            "quantity",
            "unit_estimate",
            "currency",
            "justification",
            "delivery_location",
            "requested_date",
            "budget_reference",
        ] {
            assert!(form.get(field).is_some(), "missing field {field}");
        }
        let round_trip: PurchaseRequest =
            serde_json::from_value(form).expect("structured parameters round trip");
        assert_eq!(round_trip, request());
    }
}
