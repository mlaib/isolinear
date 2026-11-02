//! UN/CEFACT Cross Industry Invoice (D16B, `CrossIndustryInvoice:100`), the syntax of
//! Factur-X, ZUGFeRD 2.x and XRechnung CII; the profile comes from the guideline parameter.
use super::support::{compare, guideline, signed_sum, sum_at, value};
use crate::inspect::{
    spec::{F, T, XmlSpec},
    xmldoc::Doc,
};

const NS: &str = "urn:un:unece:uncefact:data:standard:CrossIndustryInvoice:100";
const GUIDELINE: &str = "ExchangedDocumentContext/GuidelineSpecifiedDocumentContextParameter/ID";

macro_rules! party {
    ($group:literal, $p:literal) => {
        [
            F($group, "Name", concat!("**/", $p, "/Name")),
            F($group, "Identifier", concat!("**/", $p, "/ID")),
            F($group, "Global ID", concat!("**/", $p, "/GlobalID")),
            F(
                $group,
                "Global ID scheme",
                concat!("**/", $p, "/GlobalID/@schemeID"),
            ),
            F(
                $group,
                "VAT number",
                concat!(
                    "**/",
                    $p,
                    "/SpecifiedTaxRegistration/ID[@schemeID=VA] | **/",
                    $p,
                    "/SpecifiedTaxRegistration/ID"
                ),
            ),
            F(
                $group,
                "Legal registration",
                concat!("**/", $p, "/SpecifiedLegalOrganization/ID"),
            ),
            F(
                $group,
                "Street",
                concat!("**/", $p, "/PostalTradeAddress/LineOne"),
            ),
            F(
                $group,
                "City",
                concat!("**/", $p, "/PostalTradeAddress/CityName"),
            ),
            F(
                $group,
                "Postal code",
                concat!("**/", $p, "/PostalTradeAddress/PostcodeCode"),
            ),
            F(
                $group,
                "Country",
                concat!("**/", $p, "/PostalTradeAddress/CountryID"),
            ),
            F(
                $group,
                "Electronic address",
                concat!("**/", $p, "/URIUniversalCommunication/URIID"),
            ),
        ]
    };
}

const SELLER: [F; 11] = party!("Seller", "SellerTradeParty");
const BUYER: [F; 11] = party!("Buyer", "BuyerTradeParty");
const SUM: &str = "**/SpecifiedTradeSettlementHeaderMonetarySummation";

fn version(doc: &Doc) -> Option<String> {
    let id = value(doc, 0, GUIDELINE)?;
    Some(guideline(&id).unwrap_or(id))
}

pub static INVOICE: XmlSpec = XmlSpec {
    family: "UN/CEFACT CII",
    kind: "Cross Industry Invoice",
    matches: |root, ns| root == "CrossIndustryInvoice" && ns == NS,
    version,
    facts: &[
        F("Document", "Number", "ExchangedDocument/ID"),
        F("Document", "Type code", "ExchangedDocument/TypeCode"),
        F(
            "Document",
            "Issue date",
            "ExchangedDocument/IssueDateTime/DateTimeString",
        ),
        F("Document", "Note", "ExchangedDocument/IncludedNote/Content"),
        F("Document", "Guideline", GUIDELINE),
        F(
            "Document",
            "Business process",
            "ExchangedDocumentContext/BusinessProcessSpecifiedDocumentContextParameter/ID",
        ),
        F(
            "Document",
            "Lines",
            "count:**/IncludedSupplyChainTradeLineItem",
        ),
        F(
            "Document",
            "Buyer reference",
            "**/ApplicableHeaderTradeAgreement/BuyerReference",
        ),
        F(
            "Document",
            "Order reference",
            "**/BuyerOrderReferencedDocument/IssuerAssignedID",
        ),
        SELLER[0],
        SELLER[1],
        SELLER[2],
        SELLER[3],
        SELLER[4],
        SELLER[5],
        SELLER[6],
        SELLER[7],
        SELLER[8],
        SELLER[9],
        SELLER[10],
        BUYER[0],
        BUYER[1],
        BUYER[2],
        BUYER[3],
        BUYER[4],
        BUYER[5],
        BUYER[6],
        BUYER[7],
        BUYER[8],
        BUYER[9],
        BUYER[10],
        F(
            "Delivery",
            "Date",
            "**/ActualDeliverySupplyChainEvent/OccurrenceDateTime/DateTimeString",
        ),
        F(
            "Payment",
            "Currency",
            "**/ApplicableHeaderTradeSettlement/InvoiceCurrencyCode",
        ),
        F(
            "Payment",
            "Reference",
            "**/ApplicableHeaderTradeSettlement/PaymentReference",
        ),
        F(
            "Payment",
            "Means code",
            "**/SpecifiedTradeSettlementPaymentMeans/TypeCode",
        ),
        F(
            "Payment",
            "IBAN",
            "**/SpecifiedTradeSettlementPaymentMeans/PayeePartyCreditorFinancialAccount/IBANID",
        ),
        F(
            "Payment",
            "BIC",
            "**/SpecifiedTradeSettlementPaymentMeans/PayeeSpecifiedCreditorFinancialInstitution/BICID",
        ),
        F(
            "Payment",
            "Terms",
            "**/SpecifiedTradePaymentTerms/Description",
        ),
        F(
            "Payment",
            "Due date",
            "**/SpecifiedTradePaymentTerms/DueDateDateTime/DateTimeString",
        ),
        F(
            "Totals",
            "Sum of lines",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/LineTotalAmount",
        ),
        F(
            "Totals",
            "Charges",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/ChargeTotalAmount",
        ),
        F(
            "Totals",
            "Allowances",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/AllowanceTotalAmount",
        ),
        F(
            "Totals",
            "Total without tax",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/TaxBasisTotalAmount",
        ),
        F(
            "Totals",
            "Tax",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/TaxTotalAmount",
        ),
        F(
            "Totals",
            "Total with tax",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/GrandTotalAmount",
        ),
        F(
            "Totals",
            "Prepaid",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/TotalPrepaidAmount",
        ),
        F(
            "Totals",
            "Amount due",
            "**/SpecifiedTradeSettlementHeaderMonetarySummation/DuePayableAmount",
        ),
    ],
    tables: &[
        T {
            name: "Invoice lines",
            rows: "**/IncludedSupplyChainTradeLineItem",
            columns: &[
                ("ID", "AssociatedDocumentLineDocument/LineID"),
                ("Item", "SpecifiedTradeProduct/Name"),
                ("Global ID", "SpecifiedTradeProduct/GlobalID"),
                ("Seller item ID", "SpecifiedTradeProduct/SellerAssignedID"),
                ("Quantity", "SpecifiedLineTradeDelivery/BilledQuantity"),
                (
                    "Unit",
                    "SpecifiedLineTradeDelivery/BilledQuantity/@unitCode",
                ),
                (
                    "Net price",
                    "SpecifiedLineTradeAgreement/NetPriceProductTradePrice/ChargeAmount",
                ),
                (
                    "Line amount",
                    "SpecifiedLineTradeSettlement/SpecifiedTradeSettlementLineMonetarySummation/LineTotalAmount",
                ),
                (
                    "Tax category",
                    "SpecifiedLineTradeSettlement/ApplicableTradeTax/CategoryCode",
                ),
                (
                    "Tax percent",
                    "SpecifiedLineTradeSettlement/ApplicableTradeTax/RateApplicablePercent",
                ),
            ],
        },
        T {
            name: "Tax breakdown",
            rows: "**/ApplicableHeaderTradeSettlement/ApplicableTradeTax",
            columns: &[
                ("Type", "TypeCode"),
                ("Category", "CategoryCode"),
                ("Percent", "RateApplicablePercent"),
                ("Basis amount", "BasisAmount"),
                ("Tax amount", "CalculatedAmount"),
                ("Exemption reason", "ExemptionReason | ExemptionReasonCode"),
            ],
        },
        T {
            name: "Allowances and charges",
            rows: "**/ApplicableHeaderTradeSettlement/SpecifiedTradeAllowanceCharge",
            columns: &[
                ("Charge", "ChargeIndicator/Indicator"),
                ("Reason", "Reason | ReasonCode"),
                ("Amount", "ActualAmount"),
                ("Base amount", "BasisAmount"),
                ("Tax category", "CategoryTradeTax/CategoryCode"),
                ("Tax percent", "CategoryTradeTax/RateApplicablePercent"),
            ],
        },
    ],
    custom: Some(|doc, out| {
        let total = |p: &str| value(doc, 0, &format!("{SUM}/{p}"));
        compare(
            out,
            "Sum of line amounts (LineTotalAmount)",
            total("LineTotalAmount"),
            sum_at(
                doc,
                0,
                "**/IncludedSupplyChainTradeLineItem/SpecifiedLineTradeSettlement/SpecifiedTradeSettlementLineMonetarySummation/LineTotalAmount",
            ),
        );
        // The tax total in the invoice currency (a second one may be in the tax currency).
        let tax = value(
            doc,
            0,
            "**/ApplicableHeaderTradeSettlement/InvoiceCurrencyCode",
        )
        .and_then(|c| total(&format!("TaxTotalAmount[@currencyID={c}]")))
        .or_else(|| total("TaxTotalAmount"));
        compare(
            out,
            "Total with tax = total without tax + tax",
            total("GrandTotalAmount"),
            signed_sum(&[(true, total("TaxBasisTotalAmount")), (true, tax.clone())]),
        );
        compare(
            out,
            "Tax total = sum of the tax breakdown",
            tax,
            sum_at(
                doc,
                0,
                "**/ApplicableHeaderTradeSettlement/ApplicableTradeTax/CalculatedAmount",
            ),
        );
        if let Some(name) = value(doc, 0, GUIDELINE).and_then(|g| guideline(&g)) {
            out.kind = format!("Cross Industry Invoice ({name})");
        }
        out.notes.push(
            "A Factur-X or ZUGFeRD PDF carries this XML as an attachment; open the XML itself to see it here"
                .into(),
        );
    }),
};
