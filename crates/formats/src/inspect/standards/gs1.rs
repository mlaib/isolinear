//! GS1 XML business messages (`urn:gs1:ecom:*`, version 3) and documents wrapped in the
//! UN/CEFACT Standard Business Document Header. Orders, despatch advices and invoices are
//! listed; GTIN, GLN and SSCC values are checked with the GS1 check digit.
use super::support::{table, value};
use crate::inspect::{
    Inspection, MAX_ROWS, Row, Stage, State, Table,
    spec::{F, T, XmlSpec},
    xmldoc::Doc,
};

const SBDH_NS: &str = "http://www.unece.org/cefact/namespaces/StandardBusinessDocumentHeader";
const GS1_NS: &str = "urn:gs1:ecom:";

/// Whether the last digit of a GS1 key is its mod-10 check digit (weights 3, 1 from the right).
pub fn check_digit_ok(key: &str) -> bool {
    let digits: Vec<u32> = key.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() != key.len() || digits.len() < 2 {
        return false;
    }
    let (body, check) = digits.split_at(digits.len() - 1);
    let sum: u32 = body
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| if i % 2 == 0 { d * 3 } else { *d })
        .sum();
    (10 - sum % 10) % 10 == check[0]
}

/// The expected length of each GS1 key element.
fn key_lengths(name: &str) -> Option<&'static [usize]> {
    const GTIN: &[usize] = &[8, 12, 13, 14];
    const GLN: &[usize] = &[13];
    const SSCC: &[usize] = &[18];
    const GSIN: &[usize] = &[17];
    match name {
        "gtin" => Some(GTIN),
        "gln" => Some(GLN),
        "sscc" => Some(SSCC),
        "gsin" => Some(GSIN),
        _ => None,
    }
}

/// Every GTIN, GLN and SSCC (and GS1-authority SBDH identifiers) with its check digit.
fn identifiers(doc: &Doc, out: &mut Inspection) {
    let mut table = Table {
        name: "GS1 identifiers".into(),
        columns: ["Kind", "Value", "In", "Check digit"]
            .map(String::from)
            .to_vec(),
        rows: vec![],
        total: None,
    };
    let (mut checked, mut bad) = (0usize, vec![]);
    for (i, e) in doc.elements.iter().enumerate() {
        let i = i as u32;
        let sbdh_gs1 = e.name == "Identifier"
            && e.ns == SBDH_NS
            && doc
                .attr(i, "Authority")
                .is_some_and(|a| a.eq_ignore_ascii_case("GS1"));
        let lengths = if sbdh_gs1 {
            Some(&[13usize][..])
        } else {
            key_lengths(&e.name)
        };
        let Some(lengths) = lengths else {
            continue;
        };
        let v = doc.text(i);
        if v.is_empty() {
            continue;
        }
        let ok = lengths.contains(&v.len()) && check_digit_ok(&v);
        checked += 1;
        if !ok {
            bad.push(v.clone());
        }
        if table.rows.len() < MAX_ROWS {
            let parent = e
                .parent
                .map(|p| doc.get(p).name.clone())
                .unwrap_or_default();
            table.rows.push(Row {
                cells: vec![
                    if sbdh_gs1 {
                        "GLN".into()
                    } else {
                        e.name.to_uppercase()
                    },
                    v,
                    parent,
                    if ok { "valid" } else { "INVALID" }.into(),
                ],
                source: Some(e.span.clone()),
            });
        }
    }
    if checked == 0 {
        return;
    }
    out.check(
        Stage::Semantic,
        if bad.is_empty() {
            State::Passed
        } else {
            State::Failed
        },
        if bad.is_empty() {
            format!("GS1 check digits and lengths of {checked} identifiers are valid")
        } else {
            format!(
                "GS1 check digits or lengths: {} of {checked} identifiers are invalid ({})",
                bad.len(),
                bad.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
            )
        },
    );
    out.tables.push(table);
}

/// "despatchAdviceMessage" -> "Despatch advice".
fn readable(root: &str) -> String {
    let base = root.strip_suffix("Message").unwrap_or(root);
    let mut s = String::new();
    for (i, c) in base.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            s.push(' ');
            s.extend(c.to_lowercase());
        } else if i == 0 {
            s.extend(c.to_uppercase());
        } else {
            s.push(c);
        }
    }
    s
}

const HEADER: [F; 9] = [
    F(
        "Header",
        "Sender",
        "**/StandardBusinessDocumentHeader/Sender/Identifier",
    ),
    F(
        "Header",
        "Sender authority",
        "**/StandardBusinessDocumentHeader/Sender/Identifier/@Authority",
    ),
    F(
        "Header",
        "Receiver",
        "**/StandardBusinessDocumentHeader/Receiver/Identifier",
    ),
    F(
        "Header",
        "Standard",
        "**/StandardBusinessDocumentHeader/DocumentIdentification/Standard",
    ),
    F(
        "Header",
        "Type",
        "**/StandardBusinessDocumentHeader/DocumentIdentification/Type",
    ),
    F(
        "Header",
        "Type version",
        "**/StandardBusinessDocumentHeader/DocumentIdentification/TypeVersion",
    ),
    F(
        "Header",
        "Instance ID",
        "**/StandardBusinessDocumentHeader/DocumentIdentification/InstanceIdentifier",
    ),
    F(
        "Header",
        "Created",
        "**/StandardBusinessDocumentHeader/DocumentIdentification/CreationDateAndTime",
    ),
    F(
        "Header",
        "Header version",
        "**/StandardBusinessDocumentHeader/HeaderVersion",
    ),
];

fn gs1_custom(doc: &Doc, out: &mut Inspection) {
    let root = &doc.root().name;
    if doc.root().ns.starts_with(GS1_NS) {
        out.kind = format!("GS1 XML — {}", readable(root));
    } else if let Some(t) = value(
        doc,
        0,
        "**/StandardBusinessDocumentHeader/DocumentIdentification/Type",
    ) {
        out.kind = format!("Standard Business Document — {t}");
    }
    // Despatch logistic units: the SSCC and what they contain.
    let units = crate::inspect::spec::select(doc, 0, "**/despatchAdviceLogisticUnit");
    if let Some(t) = table(
        doc,
        "Logistic units",
        &units,
        &[
            ("SSCC", "logisticUnitIdentification/sscc"),
            ("Package type", "**/packageTypeCode"),
            ("Lines", "count:despatchAdviceLineItem"),
            ("Gross weight", "**/grossWeight"),
        ],
    ) {
        out.tables.push(t);
    }
    identifiers(doc, out);
}

pub static MESSAGE: XmlSpec = XmlSpec {
    family: "GS1 XML",
    kind: "GS1 XML business message",
    matches: |_, ns| ns.starts_with(GS1_NS),
    version: |doc| {
        let v = value(
            doc,
            0,
            "**/StandardBusinessDocumentHeader/DocumentIdentification/TypeVersion",
        );
        v.or_else(|| doc.root().ns.rsplit(':').next().map(|x| x.to_owned()))
    },
    facts: &[
        HEADER[0],
        HEADER[1],
        HEADER[2],
        HEADER[3],
        HEADER[4],
        HEADER[5],
        HEADER[6],
        HEADER[7],
        F(
            "Document",
            "Number",
            "**/orderIdentification/entityIdentification | **/despatchAdviceIdentification/entityIdentification | **/invoiceIdentification/entityIdentification | **/entityIdentification",
        ),
        F("Document", "Created", "*/creationDateTime"),
        F("Document", "Status", "*/documentStatusCode"),
        F("Document", "Order type", "*/orderTypeCode"),
        F("Document", "Invoice type", "*/invoiceType"),
        F(
            "Document",
            "Currency",
            "*/invoiceCurrencyCode | */orderCurrencyCode",
        ),
        F(
            "Document",
            "Order reference",
            "*/purchaseOrder/entityIdentification",
        ),
        F("Parties", "Buyer GLN", "*/buyer/gln"),
        F("Parties", "Seller GLN", "*/seller/gln"),
        F("Parties", "Ship to GLN", "**/shipTo/gln"),
        F("Parties", "Ship from GLN", "**/shipFrom/gln"),
        F("Parties", "Shipper GLN", "*/shipper/gln"),
        F("Parties", "Receiver GLN", "*/receiver/gln"),
        F("Parties", "Bill to GLN", "*/billTo/gln"),
        F(
            "Delivery",
            "Requested delivery",
            "**/requestedDeliveryDateTime | **/requestedDeliveryDateRange/beginDate",
        ),
        F(
            "Delivery",
            "Estimated delivery",
            "**/estimatedDeliveryDateTime/date | **/estimatedDeliveryDateTime",
        ),
        F(
            "Totals",
            "Total invoice amount",
            "**/invoiceTotals/totalInvoiceAmount",
        ),
        F("Totals", "Total tax", "**/invoiceTotals/totalTaxAmount"),
        F(
            "Totals",
            "Total amount",
            "**/totalMonetaryAmountExcludingTaxes",
        ),
    ],
    tables: &[
        T {
            name: "Order lines",
            rows: "**/orderLineItem",
            columns: &[
                ("Line", "lineItemNumber"),
                ("GTIN", "transactionalTradeItem/gtin"),
                ("Description", "transactionalTradeItem/tradeItemDescription"),
                ("Quantity", "requestedQuantity"),
                ("Unit", "requestedQuantity/@measurementUnitCode"),
                ("Net price", "netPrice"),
                ("Net amount", "netAmount"),
            ],
        },
        T {
            name: "Despatch lines",
            rows: "**/despatchAdviceLineItem",
            columns: &[
                ("Line", "lineItemNumber"),
                ("GTIN", "transactionalTradeItem/gtin"),
                ("Description", "transactionalTradeItem/tradeItemDescription"),
                ("Quantity", "despatchedQuantity"),
                ("Unit", "despatchedQuantity/@measurementUnitCode"),
                (
                    "Batch",
                    "transactionalTradeItem/transactionalItemData/batchNumber",
                ),
                ("SSCC", "../logisticUnitIdentification/sscc"),
                ("Order", "purchaseOrder/entityIdentification"),
            ],
        },
        T {
            name: "Invoice lines",
            rows: "**/invoiceLineItem",
            columns: &[
                ("Line", "lineItemNumber"),
                ("GTIN", "transactionalTradeItem/gtin"),
                ("Description", "transactionalTradeItem/tradeItemDescription"),
                ("Quantity", "invoicedQuantity"),
                ("Unit", "invoicedQuantity/@measurementUnitCode"),
                ("Price", "itemPriceExclusiveAllowancesCharges"),
                ("Amount", "amountExclusiveAllowancesCharges"),
                (
                    "Tax percent",
                    "invoiceLineTaxInformation/dutyFeeTaxPercentage",
                ),
            ],
        },
    ],
    custom: Some(gs1_custom),
};

/// Any document wrapped in a Standard Business Document Header (GS1, Peppol envelopes and
/// others): the header, and GS1 keys if any.
pub static SBD: XmlSpec = XmlSpec {
    family: "UN/CEFACT SBDH",
    kind: "Standard Business Document",
    matches: |root, ns| root == "StandardBusinessDocument" && ns == SBDH_NS,
    version: |doc| value(doc, 0, "StandardBusinessDocumentHeader/HeaderVersion"),
    facts: &[
        HEADER[0], HEADER[1], HEADER[2], HEADER[3], HEADER[4], HEADER[5], HEADER[6], HEADER[7],
        HEADER[8],
    ],
    tables: &[],
    custom: Some(|doc, out| {
        // The payload: the element after the header.
        if let Some(&p) = doc
            .root()
            .children
            .iter()
            .find(|&&c| doc.get(c).name != "StandardBusinessDocumentHeader")
        {
            let el = doc.get(p);
            out.fact(
                "Payload",
                "Root element",
                format!("{} ({})", el.name, el.ns),
                Some(el.span.clone()),
            );
            if el.ns.starts_with(GS1_NS) {
                out.family = "GS1 XML".into();
            }
        }
        gs1_custom(doc, out);
        out.notes.push(
            "Only the envelope is interpreted here; open the payload on its own for its standard's view".into(),
        );
    }),
};
