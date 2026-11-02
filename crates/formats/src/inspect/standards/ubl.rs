//! OASIS UBL 2.x invoices and credit notes, and the EN 16931 customizations carried in them
//! (Peppol BIS Billing 3.0, XRechnung), recognized from the root namespace and relabelled from
//! `cbc:CustomizationID`.
use super::support::{compare, eas_name, guideline, signed_sum, sum_at, value};
use crate::inspect::{
    Inspection,
    spec::{F, T, XmlSpec},
    xmldoc::Doc,
};

const INVOICE_NS: &str = "urn:oasis:names:specification:ubl:schema:xsd:Invoice-2";
const CREDIT_NOTE_NS: &str = "urn:oasis:names:specification:ubl:schema:xsd:CreditNote-2";

fn version(doc: &Doc) -> Option<String> {
    let ubl = value(doc, 0, "UBLVersionID").map(|v| format!("UBL {v}"));
    let custom = value(doc, 0, "CustomizationID").and_then(|c| guideline(&c));
    match (ubl, custom) {
        (Some(u), Some(c)) => Some(format!("{u}, {c}")),
        (u, c) => u.or(c),
    }
}

/// Name, identifiers, VAT number and address of a party.
macro_rules! party {
    ($group:literal, $p:literal) => {
        [
            F(
                $group,
                "Name",
                concat!(
                    $p,
                    "/Party/PartyName/Name | ",
                    $p,
                    "/Party/PartyLegalEntity/RegistrationName"
                ),
            ),
            F(
                $group,
                "Legal name",
                concat!($p, "/Party/PartyLegalEntity/RegistrationName"),
            ),
            F($group, "Endpoint ID", concat!($p, "/Party/EndpointID")),
            F(
                $group,
                "Endpoint scheme",
                concat!($p, "/Party/EndpointID/@schemeID"),
            ),
            F(
                $group,
                "Identifier",
                concat!($p, "/Party/PartyIdentification/ID"),
            ),
            F(
                $group,
                "VAT number",
                concat!(
                    $p,
                    "/Party/PartyTaxScheme[TaxScheme/ID=VAT]/CompanyID | ",
                    $p,
                    "/Party/PartyTaxScheme/CompanyID"
                ),
            ),
            F(
                $group,
                "Company ID",
                concat!($p, "/Party/PartyLegalEntity/CompanyID"),
            ),
            F(
                $group,
                "Street",
                concat!($p, "/Party/PostalAddress/StreetName"),
            ),
            F($group, "City", concat!($p, "/Party/PostalAddress/CityName")),
            F(
                $group,
                "Postal code",
                concat!($p, "/Party/PostalAddress/PostalZone"),
            ),
            F(
                $group,
                "Country",
                concat!($p, "/Party/PostalAddress/Country/IdentificationCode"),
            ),
            F(
                $group,
                "Contact e-mail",
                concat!($p, "/Party/Contact/ElectronicMail"),
            ),
        ]
    };
}

const SUPPLIER: [F; 12] = party!("Supplier", "AccountingSupplierParty");
const CUSTOMER: [F; 12] = party!("Customer", "AccountingCustomerParty");

const PAYMENT: [F; 7] = [
    F("Payment", "Means code", "PaymentMeans/PaymentMeansCode"),
    F("Payment", "Payment ID", "PaymentMeans/PaymentID"),
    F("Payment", "IBAN", "PaymentMeans/PayeeFinancialAccount/ID"),
    F(
        "Payment",
        "Account name",
        "PaymentMeans/PayeeFinancialAccount/Name",
    ),
    F(
        "Payment",
        "BIC",
        "PaymentMeans/PayeeFinancialAccount/FinancialInstitutionBranch/ID",
    ),
    F(
        "Payment",
        "Mandate (direct debit)",
        "PaymentMeans/PaymentMandate/ID",
    ),
    F("Payment", "Terms", "PaymentTerms/Note"),
];

const TOTALS: [F; 10] = [
    F(
        "Totals",
        "Sum of lines",
        "LegalMonetaryTotal/LineExtensionAmount",
    ),
    F(
        "Totals",
        "Allowances",
        "LegalMonetaryTotal/AllowanceTotalAmount",
    ),
    F("Totals", "Charges", "LegalMonetaryTotal/ChargeTotalAmount"),
    F(
        "Totals",
        "Total without tax",
        "LegalMonetaryTotal/TaxExclusiveAmount",
    ),
    F("Totals", "Tax", "TaxTotal/TaxAmount"),
    F(
        "Totals",
        "Total with tax",
        "LegalMonetaryTotal/TaxInclusiveAmount",
    ),
    F("Totals", "Prepaid", "LegalMonetaryTotal/PrepaidAmount"),
    F(
        "Totals",
        "Rounding",
        "LegalMonetaryTotal/PayableRoundingAmount",
    ),
    F("Totals", "Amount due", "LegalMonetaryTotal/PayableAmount"),
    F(
        "Totals",
        "Currency",
        "LegalMonetaryTotal/PayableAmount/@currencyID",
    ),
];

const TAX_BREAKDOWN: T = T {
    name: "Tax breakdown",
    rows: "TaxTotal/TaxSubtotal",
    columns: &[
        ("Category", "TaxCategory/ID"),
        ("Percent", "TaxCategory/Percent"),
        ("Taxable amount", "TaxableAmount"),
        ("Tax amount", "TaxAmount"),
        ("Currency", "TaxAmount/@currencyID"),
        (
            "Exemption reason",
            "TaxCategory/TaxExemptionReason | TaxCategory/TaxExemptionReasonCode",
        ),
        ("Scheme", "TaxCategory/TaxScheme/ID"),
    ],
};

const ALLOWANCES: T = T {
    name: "Allowances and charges",
    rows: "AllowanceCharge",
    columns: &[
        ("Charge", "ChargeIndicator"),
        (
            "Reason",
            "AllowanceChargeReason | AllowanceChargeReasonCode",
        ),
        ("Amount", "Amount"),
        ("Base amount", "BaseAmount"),
        ("Tax category", "TaxCategory/ID"),
        ("Tax percent", "TaxCategory/Percent"),
    ],
};

/// The line columns, with the quantity element of the document type.
macro_rules! lines {
    ($name:literal, $rows:literal, $qty:literal) => {
        T {
            name: $name,
            rows: $rows,
            columns: &[
                ("ID", "ID"),
                ("Item", "Item/Name"),
                ("Description", "Item/Description"),
                ("Quantity", $qty),
                ("Unit", concat!($qty, "/@unitCode")),
                ("Price", "Price/PriceAmount"),
                ("Line amount", "LineExtensionAmount"),
                ("Currency", "LineExtensionAmount/@currencyID"),
                ("Tax category", "Item/ClassifiedTaxCategory/ID"),
                ("Tax percent", "Item/ClassifiedTaxCategory/Percent"),
                ("Seller item ID", "Item/SellersItemIdentification/ID"),
                ("Standard item ID", "Item/StandardItemIdentification/ID"),
                ("Note", "Note"),
            ],
        }
    };
}

/// EN 16931 totals rules on declared amounts, computed exactly.
fn totals_checks(doc: &Doc, out: &mut Inspection, line: &str) {
    let total = |p: &str| value(doc, 0, &format!("LegalMonetaryTotal/{p}"));
    // BR-CO-10: sum of line net amounts.
    compare(
        out,
        "Sum of line amounts (LineExtensionAmount)",
        total("LineExtensionAmount"),
        sum_at(doc, 0, &format!("{line}/LineExtensionAmount")),
    );
    // BR-CO-13: lines - allowances + charges = total without tax.
    let lines = total("LineExtensionAmount");
    let allowances = total("AllowanceTotalAmount").or(Some("0".into()));
    let charges = total("ChargeTotalAmount").or(Some("0".into()));
    compare(
        out,
        "Total without tax = lines − allowances + charges",
        total("TaxExclusiveAmount"),
        signed_sum(&[(true, lines), (false, allowances), (true, charges)]),
    );
    // BR-CO-15: total without tax + tax = total with tax (tax in the document currency).
    let currency = value(doc, 0, "DocumentCurrencyCode");
    let tax = currency
        .as_ref()
        .and_then(|c| {
            value(
                doc,
                0,
                &format!("TaxTotal[TaxAmount/@currencyID={c}]/TaxAmount"),
            )
        })
        .or_else(|| value(doc, 0, "TaxTotal/TaxAmount"));
    compare(
        out,
        "Total with tax = total without tax + tax",
        total("TaxInclusiveAmount"),
        signed_sum(&[(true, total("TaxExclusiveAmount")), (true, tax.clone())]),
    );
    // BR-CO-14: tax total = sum of the breakdown's tax amounts.
    compare(
        out,
        "Tax total = sum of the tax breakdown",
        tax,
        sum_at(doc, 0, "TaxTotal/TaxSubtotal/TaxAmount"),
    );
    // BR-CO-16: amount due = total with tax - prepaid + rounding.
    compare(
        out,
        "Amount due = total with tax − prepaid + rounding",
        total("PayableAmount"),
        signed_sum(&[
            (true, total("TaxInclusiveAmount")),
            (false, total("PrepaidAmount").or(Some("0".into()))),
            (true, total("PayableRoundingAmount").or(Some("0".into()))),
        ]),
    );
}

/// Peppol and other EN 16931 customizations: family, kind and endpoint facts.
fn customization(doc: &Doc, out: &mut Inspection, document: &str) {
    let Some(id) = value(doc, 0, "CustomizationID") else {
        out.notes
            .push("No CustomizationID: the profile of this document is not stated".into());
        return;
    };
    let Some(name) = guideline(&id) else {
        return;
    };
    if name.starts_with("Peppol") {
        out.family = "Peppol BIS Billing 3.0".into();
        out.kind = format!("{document} (UBL, Peppol BIS Billing 3.0)");
        out.version = Some("3.0".into());
        let profile = value(doc, 0, "ProfileID").unwrap_or_default();
        let profile_name = match profile.trim() {
            "urn:fdc:peppol.eu:2017:poacc:billing:01:1.0" => "Billing (P01), version 1.0",
            "" => "",
            _ => "Not the Peppol billing process",
        };
        out.fact("Peppol", "Profile", profile_name, None);
        for (label, party) in [
            ("Seller endpoint", "AccountingSupplierParty"),
            ("Buyer endpoint", "AccountingCustomerParty"),
        ] {
            let id = value(doc, 0, &format!("{party}/Party/EndpointID"));
            let scheme = value(doc, 0, &format!("{party}/Party/EndpointID/@schemeID"));
            if let Some(id) = id {
                let shown = match scheme {
                    Some(s) => match eas_name(&s) {
                        Some(n) => format!("{s}:{id} ({n})"),
                        None => format!("{s}:{id}"),
                    },
                    None => id,
                };
                out.fact("Peppol", label, shown, None);
            }
        }
    } else {
        out.kind = format!("{} ({name})", out.kind);
    }
}

pub static INVOICE: XmlSpec = XmlSpec {
    family: "UBL",
    kind: "Invoice",
    matches: |root, ns| root == "Invoice" && ns == INVOICE_NS,
    version,
    facts: &[
        F("Invoice", "Number", "ID"),
        F("Invoice", "Issue date", "IssueDate"),
        F(
            "Invoice",
            "Due date",
            "DueDate | PaymentMeans/PaymentDueDate",
        ),
        F("Invoice", "Type code", "InvoiceTypeCode"),
        F("Invoice", "Currency", "DocumentCurrencyCode"),
        F("Invoice", "Tax currency", "TaxCurrencyCode"),
        F("Invoice", "Buyer reference", "BuyerReference"),
        F("Invoice", "Order reference", "OrderReference/ID"),
        F("Invoice", "Contract", "ContractDocumentReference/ID"),
        F("Invoice", "Note", "Note"),
        F("Invoice", "Customization", "CustomizationID"),
        F("Invoice", "Profile", "ProfileID"),
        F("Invoice", "Lines", "count:InvoiceLine"),
        SUPPLIER[0],
        SUPPLIER[1],
        SUPPLIER[2],
        SUPPLIER[3],
        SUPPLIER[4],
        SUPPLIER[5],
        SUPPLIER[6],
        SUPPLIER[7],
        SUPPLIER[8],
        SUPPLIER[9],
        SUPPLIER[10],
        SUPPLIER[11],
        CUSTOMER[0],
        CUSTOMER[1],
        CUSTOMER[2],
        CUSTOMER[3],
        CUSTOMER[4],
        CUSTOMER[5],
        CUSTOMER[6],
        CUSTOMER[7],
        CUSTOMER[8],
        CUSTOMER[9],
        CUSTOMER[10],
        CUSTOMER[11],
        F("Delivery", "Date", "Delivery/ActualDeliveryDate"),
        F("Delivery", "Location", "Delivery/DeliveryLocation/ID"),
        PAYMENT[0],
        PAYMENT[1],
        PAYMENT[2],
        PAYMENT[3],
        PAYMENT[4],
        PAYMENT[5],
        PAYMENT[6],
        TOTALS[0],
        TOTALS[1],
        TOTALS[2],
        TOTALS[3],
        TOTALS[4],
        TOTALS[5],
        TOTALS[6],
        TOTALS[7],
        TOTALS[8],
        TOTALS[9],
    ],
    tables: &[
        lines!("Invoice lines", "InvoiceLine", "InvoicedQuantity"),
        TAX_BREAKDOWN,
        ALLOWANCES,
    ],
    custom: Some(|doc, out| {
        totals_checks(doc, out, "InvoiceLine");
        customization(doc, out, "Invoice");
    }),
};

pub static CREDIT_NOTE: XmlSpec = XmlSpec {
    family: "UBL",
    kind: "Credit note",
    matches: |root, ns| root == "CreditNote" && ns == CREDIT_NOTE_NS,
    version,
    facts: &[
        F("Credit note", "Number", "ID"),
        F("Credit note", "Issue date", "IssueDate"),
        F(
            "Credit note",
            "Due date",
            "DueDate | PaymentMeans/PaymentDueDate",
        ),
        F("Credit note", "Type code", "CreditNoteTypeCode"),
        F("Credit note", "Currency", "DocumentCurrencyCode"),
        F("Credit note", "Buyer reference", "BuyerReference"),
        F("Credit note", "Note", "Note"),
        F("Credit note", "Customization", "CustomizationID"),
        F("Credit note", "Profile", "ProfileID"),
        F("Credit note", "Lines", "count:CreditNoteLine"),
        F(
            "Original invoice",
            "Number",
            "BillingReference/InvoiceDocumentReference/ID",
        ),
        F(
            "Original invoice",
            "Issue date",
            "BillingReference/InvoiceDocumentReference/IssueDate",
        ),
        F("Credit note", "Reason", "DiscrepancyResponse/Description"),
        SUPPLIER[0],
        SUPPLIER[1],
        SUPPLIER[2],
        SUPPLIER[3],
        SUPPLIER[4],
        SUPPLIER[5],
        SUPPLIER[6],
        SUPPLIER[7],
        SUPPLIER[8],
        SUPPLIER[9],
        SUPPLIER[10],
        SUPPLIER[11],
        CUSTOMER[0],
        CUSTOMER[1],
        CUSTOMER[2],
        CUSTOMER[3],
        CUSTOMER[4],
        CUSTOMER[5],
        CUSTOMER[6],
        CUSTOMER[7],
        CUSTOMER[8],
        CUSTOMER[9],
        CUSTOMER[10],
        CUSTOMER[11],
        PAYMENT[0],
        PAYMENT[1],
        PAYMENT[2],
        PAYMENT[3],
        PAYMENT[4],
        PAYMENT[5],
        PAYMENT[6],
        TOTALS[0],
        TOTALS[1],
        TOTALS[2],
        TOTALS[3],
        TOTALS[4],
        TOTALS[5],
        TOTALS[6],
        TOTALS[7],
        TOTALS[8],
        TOTALS[9],
    ],
    tables: &[
        lines!("Credit note lines", "CreditNoteLine", "CreditedQuantity"),
        TAX_BREAKDOWN,
        ALLOWANCES,
    ],
    custom: Some(|doc, out| {
        totals_checks(doc, out, "CreditNoteLine");
        customization(doc, out, "Credit note");
    }),
};
