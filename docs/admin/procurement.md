# Governed procurement workflow

An employee asks: "采购 20 台 27 寸显示器,上海办公室使用"(Purchase 20 monitors
for the Shanghai office). OpenWork resolves the request fields against a
read-only ERP/budget capability, prepares a structured purchase request, and
classifies the final ERP write as an approval-required action (`AwaitingApproval`)
until an authorized manager approves or denies the exact request. The demo
backend is `org.openwork.erp.procurement` with a deterministic mock ERP adapter;
a real ERP (SAP, Odoo, Kingdee, Yonyou) plugs in without changing policy or
approval semantics.

## Capabilities

| Capability | Risk | Behavior |
|---|---|---|
| `erp.budget.read` | L0 | Reads remaining budget for one department. Immediate, read-only. |
| `erp.vendor.read` | L0 | Searches approved vendors. Immediate, read-only. |
| `erp.item.read` | L0 | Searches the catalog. Immediate, read-only. |
| `erp.purchase_request.create` | L3 | Creates an exact structured purchase request **only after** single-use manager approval. |

The request payload carries the full data model: requester, department, cost
center, item description, quantity, unit estimate, currency, vendor or vendor
constraints, business justification, delivery location, requested date, and
budget reference. The mock backend validates quantity/estimate bounds, rejects
unapproved vendors, and fails closed when the department budget is insufficient.

## Execution path

1. `POST /v1/apps/org.openwork.erp.procurement/capabilities/erp.purchase_request.create/invoke`
   enters `AwaitingApproval` with exact parameter binding (the approval is bound
   to the parameter hash; parameters are re-validated at execution and can never
   be swapped).
2. The manager approves or denies the exact request via
   `POST /v1/approvals/{id}/approve|deny` (or the Employee Workspace Approvals
   page, which renders the full request).
3. Only an `Approved` approval can be executed; execution consumes the claim
   (single-use, replay rejected) then calls the ERP adapter through
   `ErpAdapter::create_purchase_request`.
4. Denial causes no ERP write and the run stays `awaiting_approval`.
5. Success returns a stable mock ERP reference (`PR-2026-0001`, …) and attaches
   generated artifacts: a Markdown purchase request and an XLSX spreadsheet.
6. The full audit trail records request, policy decision, approval request,
   approval/denial, claim consumption, ERP execution, artifacts, and
   completion — all without credential material.

## The ERP adapter boundary

`crates/openwork-control-api/src/erp.rs` defines `ErpAdapter` with read-only
budget/vendor/item queries and purchase-request creation. The deterministic
`MockErpAdapter` (budget constants per department, fixed approved vendors,
catalog, sequential request IDs from `PR-2026-0001`) is the local/demo backend.
A real ERP adapter implements the same trait or wraps an ERP client container;
governance, approval, and credential boundaries never change. Any production
adapter receives credentials only through the reviewed credential boundary
(`docs/admin/credentials.md`) and never broader permissions than the reviewed
capability requires.

## Contract verification

The real-Postgres test `procurement_flow_governs_exact_approval_and_generates_artifacts`
(in `openwork-control-api` lib tests) walks the whole flow against a disposable
database: invoke → `awaiting_approval` → denial never writes → approval →
single-use execution → `PR-2026-0001` → markdown/xlsx artifacts → replay
rejected → redacted audit trail. Run it with:

```bash
OPENWORK_TEST_DATABASE_URL=postgres://openwork:...@127.0.0.1:5432/openwork_test \
OPENWORK_TEST_DATABASE_RESET=1 \
cargo test -p openwork-control-api --lib procurement_flow
```

## Demo flow

```text
Employee: "申请采购 20 台 27 寸显示器,上海办公室使用"
OpenWork: reads remaining budget and approved vendors (L0)
OpenWork: prepares structured request + Markdown/XLSX artifacts
OpenWork: purchase request requires manager approval (AwaitingApproval)
Manager: approves exact request
OpenWork: creates mock ERP purchase request PR-2026-0001
Employee: sees ERP reference + generated artifact preview
Admin: sees complete audit trail; no raw credentials anywhere
```
