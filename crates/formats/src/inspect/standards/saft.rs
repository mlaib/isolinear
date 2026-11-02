//! Standard Audit File for Tax: the OECD schema and its national variants (Portugal 1.04,
//! Norway Financial), and the Polish JPK files. Header, master files, general ledger entries and
//! source documents are listed; debits and credits are added up exactly.
use super::support::{compare, sum_at, value};
use crate::inspect::{
    Inspection, Stage, State,
    spec::{F, T, XmlSpec, decimal_eq, decimal_sum, select},
    xmldoc::Doc,
};

const OECD: &str = "urn:OECD:StandardAuditFile-Tax";
const PT: &str = "urn:OECD:StandardAuditFile-Tax:PT_";
const NO: &str = "urn:StandardAuditFile-Taxation-Financial:NO";

fn version(doc: &Doc) -> Option<String> {
    value(doc, 0, "Header/AuditFileVersion").or_else(|| {
        let ns = &doc.root().ns;
        ns.rsplit(':').next().map(str::to_owned)
    })
}

/// Debit and credit amounts of one transaction (paths from the transaction); empty when an
/// amount is not a number.
fn transaction_sums(doc: &Doc, t: u32, debit: &str, credit: &str) -> (String, String) {
    let sum = |p: &str| match select(doc, t, p).len() {
        0 => "0".to_owned(),
        _ => sum_at(doc, t, p).unwrap_or_default(),
    };
    (sum(debit), sum(credit))
}

/// Debits = credits in every transaction and in total, and the declared totals and count.
fn ledger_checks(doc: &Doc, out: &mut Inspection, debit: &str, credit: &str) {
    let transactions = select(doc, 0, "GeneralLedgerEntries/Journal/Transaction");
    if transactions.is_empty() {
        return;
    }
    let mut unbalanced = vec![];
    let (mut debits, mut credits) = (vec![], vec![]);
    for &t in &transactions {
        let (d, c) = transaction_sums(doc, t, debit, credit);
        if d.is_empty() || c.is_empty() || !decimal_eq(&d, &c) {
            unbalanced.push(value(doc, t, "TransactionID").unwrap_or_else(|| "?".into()));
        }
        debits.push(d);
        credits.push(c);
    }
    let n = transactions.len();
    out.check(
        Stage::Semantic,
        if unbalanced.is_empty() {
            State::Passed
        } else {
            State::Failed
        },
        if unbalanced.is_empty() {
            format!("Debits equal credits in all {n} transactions")
        } else {
            format!(
                "Debits differ from credits in {} of {n} transactions: {}",
                unbalanced.len(),
                unbalanced
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    );
    let total_debit = decimal_sum(debits.iter().map(String::as_str));
    let total_credit = decimal_sum(credits.iter().map(String::as_str));
    compare(
        out,
        "Total debits = total credits",
        total_debit.clone(),
        total_credit.clone(),
    );
    compare(
        out,
        "Declared total debit",
        value(doc, 0, "GeneralLedgerEntries/TotalDebit"),
        total_debit,
    );
    compare(
        out,
        "Declared total credit",
        value(doc, 0, "GeneralLedgerEntries/TotalCredit"),
        total_credit,
    );
    compare(
        out,
        "Declared number of entries",
        value(doc, 0, "GeneralLedgerEntries/NumberOfEntries"),
        Some(n.to_string()),
    );
}

const HEADER_COMMON: [F; 6] = [
    F("Header", "Audit file version", "Header/AuditFileVersion"),
    F(
        "Header",
        "Created",
        "Header/AuditFileDateCreated | Header/DateCreated",
    ),
    F(
        "Software",
        "Company",
        "Header/SoftwareCompanyName | Header/ProductCompanyTaxID",
    ),
    F(
        "Software",
        "Product",
        "Header/SoftwareID | Header/ProductID",
    ),
    F(
        "Software",
        "Version",
        "Header/SoftwareVersion | Header/ProductVersion",
    ),
    F("Header", "Entries", "GeneralLedgerEntries/NumberOfEntries"),
];

pub static PORTUGAL: XmlSpec = XmlSpec {
    family: "SAF-T",
    kind: "SAF-T (PT) — Portuguese audit file",
    matches: |root, ns| root == "AuditFile" && ns.starts_with(PT),
    version,
    facts: &[
        HEADER_COMMON[0],
        HEADER_COMMON[1],
        F("Company", "Name", "Header/CompanyName"),
        F("Company", "Business name", "Header/BusinessName"),
        F("Company", "Company ID", "Header/CompanyID"),
        F(
            "Company",
            "Tax registration",
            "Header/TaxRegistrationNumber",
        ),
        F("Company", "City", "Header/CompanyAddress/City"),
        F("Company", "Country", "Header/CompanyAddress/Country"),
        F("Period", "Fiscal year", "Header/FiscalYear"),
        F("Period", "Start", "Header/StartDate"),
        F("Period", "End", "Header/EndDate"),
        F("Period", "Accounting basis", "Header/TaxAccountingBasis"),
        F("Period", "Currency", "Header/CurrencyCode"),
        HEADER_COMMON[2],
        HEADER_COMMON[3],
        HEADER_COMMON[4],
        F(
            "Software",
            "Certificate number",
            "Header/SoftwareCertificateNumber",
        ),
        F("Ledger", "Entries", "GeneralLedgerEntries/NumberOfEntries"),
        F("Ledger", "Total debit", "GeneralLedgerEntries/TotalDebit"),
        F("Ledger", "Total credit", "GeneralLedgerEntries/TotalCredit"),
        F(
            "Sales invoices",
            "Entries",
            "SourceDocuments/SalesInvoices/NumberOfEntries",
        ),
        F(
            "Sales invoices",
            "Total debit",
            "SourceDocuments/SalesInvoices/TotalDebit",
        ),
        F(
            "Sales invoices",
            "Total credit",
            "SourceDocuments/SalesInvoices/TotalCredit",
        ),
        F(
            "Payments",
            "Entries",
            "SourceDocuments/Payments/NumberOfEntries",
        ),
    ],
    tables: &[
        T {
            name: "General ledger accounts",
            rows: "MasterFiles/GeneralLedgerAccounts/Account",
            columns: &[
                ("Account", "AccountID"),
                ("Description", "AccountDescription"),
                ("Opening debit", "OpeningDebitBalance"),
                ("Opening credit", "OpeningCreditBalance"),
                ("Closing debit", "ClosingDebitBalance"),
                ("Closing credit", "ClosingCreditBalance"),
                ("Grouping", "GroupingCategory"),
                ("Taxonomy code", "TaxonomyCode"),
            ],
        },
        T {
            name: "Customers",
            rows: "MasterFiles/Customer",
            columns: &[
                ("Customer", "CustomerID"),
                ("Name", "CompanyName"),
                ("Tax ID", "CustomerTaxID"),
                ("Account", "AccountID"),
                ("City", "BillingAddress/City"),
                ("Country", "BillingAddress/Country"),
            ],
        },
        T {
            name: "Suppliers",
            rows: "MasterFiles/Supplier",
            columns: &[
                ("Supplier", "SupplierID"),
                ("Name", "CompanyName"),
                ("Tax ID", "SupplierTaxID"),
                ("Account", "AccountID"),
                ("City", "BillingAddress/City"),
                ("Country", "BillingAddress/Country"),
            ],
        },
        T {
            name: "Products",
            rows: "MasterFiles/Product",
            columns: &[
                ("Type", "ProductType"),
                ("Code", "ProductCode"),
                ("Description", "ProductDescription"),
                ("Number code", "ProductNumberCode"),
            ],
        },
        T {
            name: "Tax table",
            rows: "MasterFiles/TaxTable/TaxTableEntry",
            columns: &[
                ("Type", "TaxType"),
                ("Region", "TaxCountryRegion"),
                ("Code", "TaxCode"),
                ("Description", "Description"),
                ("Percent", "TaxPercentage"),
                ("Amount", "TaxAmount"),
            ],
        },
        T {
            name: "Ledger transactions",
            rows: "GeneralLedgerEntries/Journal/Transaction",
            columns: &[
                ("Transaction", "TransactionID"),
                ("Journal", "../JournalID"),
                ("Period", "Period"),
                ("Date", "TransactionDate"),
                ("Type", "TransactionType"),
                ("Description", "Description"),
                ("Lines", "count:Lines/*"),
                ("Debit", "sum:Lines/DebitLine/DebitAmount"),
                ("Credit", "sum:Lines/CreditLine/CreditAmount"),
            ],
        },
        T {
            name: "Ledger lines",
            rows: "GeneralLedgerEntries/Journal/Transaction/Lines/*",
            columns: &[
                ("Record", "RecordID"),
                ("Transaction", "../../TransactionID"),
                ("Account", "AccountID"),
                ("Description", "Description"),
                ("Debit", "DebitAmount"),
                ("Credit", "CreditAmount"),
                ("Source document", "SourceDocumentID"),
            ],
        },
        T {
            name: "Sales invoices",
            rows: "SourceDocuments/SalesInvoices/Invoice",
            columns: &[
                ("Number", "InvoiceNo"),
                ("ATCUD", "ATCUD"),
                ("Status", "DocumentStatus/InvoiceStatus"),
                ("Date", "InvoiceDate"),
                ("Type", "InvoiceType"),
                ("Customer", "CustomerID"),
                ("Net total", "DocumentTotals/NetTotal"),
                ("Tax", "DocumentTotals/TaxPayable"),
                ("Gross total", "DocumentTotals/GrossTotal"),
                ("Lines", "count:Line"),
            ],
        },
        T {
            name: "Payments",
            rows: "SourceDocuments/Payments/Payment",
            columns: &[
                ("Number", "PaymentRefNo"),
                ("Date", "TransactionDate"),
                ("Type", "PaymentType"),
                ("Status", "DocumentStatus/PaymentStatus"),
                ("Customer", "CustomerID"),
                ("Method", "PaymentMethod/PaymentMechanism"),
                ("Amount", "PaymentMethod/PaymentAmount"),
                ("Gross total", "DocumentTotals/GrossTotal"),
            ],
        },
    ],
    custom: Some(|doc, out| {
        ledger_checks(
            doc,
            out,
            "Lines/DebitLine/DebitAmount",
            "Lines/CreditLine/CreditAmount",
        );
        // Sales invoice totals exclude cancelled (A) and invoiced (F) documents.
        let invoices = select(doc, 0, "SourceDocuments/SalesInvoices/Invoice");
        if !invoices.is_empty() {
            let (mut debits, mut credits) = (vec![], vec![]);
            for &i in &invoices {
                let status = value(doc, i, "DocumentStatus/InvoiceStatus").unwrap_or_default();
                if status == "A" || status == "F" {
                    continue;
                }
                debits.extend(
                    select(doc, i, "Line/DebitAmount")
                        .iter()
                        .map(|&e| doc.text(e)),
                );
                credits.extend(
                    select(doc, i, "Line/CreditAmount")
                        .iter()
                        .map(|&e| doc.text(e)),
                );
            }
            let sum = |v: &Vec<String>| {
                if v.is_empty() {
                    Some("0".to_owned())
                } else {
                    decimal_sum(v.iter().map(String::as_str))
                }
            };
            compare(
                out,
                "Sales invoices: declared total credit (lines, without cancelled documents)",
                value(doc, 0, "SourceDocuments/SalesInvoices/TotalCredit"),
                sum(&credits),
            );
            compare(
                out,
                "Sales invoices: declared total debit (lines, without cancelled documents)",
                value(doc, 0, "SourceDocuments/SalesInvoices/TotalDebit"),
                sum(&debits),
            );
            compare(
                out,
                "Sales invoices: declared number of entries",
                value(doc, 0, "SourceDocuments/SalesInvoices/NumberOfEntries"),
                Some(invoices.len().to_string()),
            );
            if select(doc, 0, "SourceDocuments/**/Hash")
                .iter()
                .any(|&h| !doc.text(h).is_empty())
            {
                out.check(
                    Stage::Signature,
                    State::NotDone,
                    "Document hashes (RSA signatures over invoice data) were not verified",
                );
            }
        }
    }),
};

pub static OECD_SAFT: XmlSpec = XmlSpec {
    family: "SAF-T",
    kind: "SAF-T — OECD Standard Audit File for Tax",
    matches: |root, ns| {
        root == "AuditFile"
            && (ns.starts_with(OECD) || ns.starts_with(NO) || ns.contains("StandardAuditFile"))
    },
    version,
    facts: &[
        HEADER_COMMON[0],
        F("Header", "Country", "Header/AuditFileCountry"),
        HEADER_COMMON[1],
        F("Company", "Name", "Header/Company/Name"),
        F(
            "Company",
            "Registration number",
            "Header/Company/RegistrationNumber",
        ),
        F(
            "Company",
            "Tax registration",
            "Header/Company/TaxRegistration/TaxRegistrationNumber",
        ),
        F("Company", "City", "Header/Company/Address/City"),
        F("Company", "Country", "Header/Company/Address/Country"),
        F("Period", "Currency", "Header/DefaultCurrencyCode"),
        F(
            "Period",
            "Start",
            "Header/SelectionCriteria/PeriodStart | Header/SelectionCriteria/SelectionStartDate",
        ),
        F(
            "Period",
            "Start year",
            "Header/SelectionCriteria/PeriodStartYear",
        ),
        F(
            "Period",
            "End",
            "Header/SelectionCriteria/PeriodEnd | Header/SelectionCriteria/SelectionEndDate",
        ),
        F(
            "Period",
            "End year",
            "Header/SelectionCriteria/PeriodEndYear",
        ),
        F("Period", "Accounting basis", "Header/TaxAccountingBasis"),
        HEADER_COMMON[2],
        HEADER_COMMON[3],
        HEADER_COMMON[4],
        F("Ledger", "Entries", "GeneralLedgerEntries/NumberOfEntries"),
        F("Ledger", "Total debit", "GeneralLedgerEntries/TotalDebit"),
        F("Ledger", "Total credit", "GeneralLedgerEntries/TotalCredit"),
    ],
    tables: &[
        T {
            name: "General ledger accounts",
            rows: "MasterFiles/GeneralLedgerAccounts/Account",
            columns: &[
                ("Account", "AccountID"),
                ("Description", "AccountDescription"),
                ("Standard account", "StandardAccountID"),
                ("Type", "AccountType"),
                ("Opening debit", "OpeningDebitBalance"),
                ("Opening credit", "OpeningCreditBalance"),
                ("Closing debit", "ClosingDebitBalance"),
                ("Closing credit", "ClosingCreditBalance"),
            ],
        },
        T {
            name: "Customers",
            rows: "MasterFiles/**/Customer",
            columns: &[
                ("Customer", "CustomerID"),
                ("Name", "Name | CompanyStructure/Name"),
                (
                    "Registration",
                    "RegistrationNumber | TaxRegistration/TaxRegistrationNumber",
                ),
                ("Account", "AccountID"),
                ("City", "Address/City"),
                ("Country", "Address/Country"),
            ],
        },
        T {
            name: "Suppliers",
            rows: "MasterFiles/**/Supplier",
            columns: &[
                ("Supplier", "SupplierID"),
                ("Name", "Name | CompanyStructure/Name"),
                (
                    "Registration",
                    "RegistrationNumber | TaxRegistration/TaxRegistrationNumber",
                ),
                ("Account", "AccountID"),
                ("City", "Address/City"),
                ("Country", "Address/Country"),
            ],
        },
        T {
            name: "Tax table",
            rows: "MasterFiles/TaxTable/TaxTableEntry/TaxCodeDetails",
            columns: &[
                ("Type", "../TaxType"),
                ("Code", "TaxCode"),
                ("Description", "Description"),
                ("Percent", "TaxPercentage"),
                ("Country", "Country"),
                ("Standard code", "StandardTaxCode"),
            ],
        },
        T {
            name: "Ledger transactions",
            rows: "GeneralLedgerEntries/Journal/Transaction",
            columns: &[
                ("Transaction", "TransactionID"),
                ("Journal", "../JournalID"),
                ("Period", "Period"),
                ("Year", "PeriodYear"),
                ("Date", "TransactionDate"),
                ("Description", "Description"),
                ("Lines", "count:Line"),
                ("Debit", "sum:Line/DebitAmount/Amount"),
                ("Credit", "sum:Line/CreditAmount/Amount"),
            ],
        },
        T {
            name: "Ledger lines",
            rows: "GeneralLedgerEntries/Journal/Transaction/Line",
            columns: &[
                ("Record", "RecordID"),
                ("Transaction", "../TransactionID"),
                ("Account", "AccountID"),
                ("Description", "Description"),
                ("Debit", "DebitAmount/Amount"),
                ("Credit", "CreditAmount/Amount"),
                ("Customer", "CustomerID"),
                ("Supplier", "SupplierID"),
                ("Tax code", "TaxInformation/TaxCode"),
            ],
        },
        T {
            name: "Sales invoices",
            rows: "SourceDocuments/SalesInvoices/Invoice",
            columns: &[
                ("Number", "InvoiceNo"),
                ("Date", "InvoiceDate"),
                ("Type", "InvoiceType"),
                ("Customer", "CustomerInfo/CustomerID"),
                ("Net total", "DocumentTotals/NetTotal"),
                ("Tax", "DocumentTotals/TaxPayable"),
                ("Gross total", "DocumentTotals/GrossTotal"),
            ],
        },
        T {
            name: "Payments",
            rows: "SourceDocuments/Payments/Payment",
            columns: &[
                ("Number", "PaymentRefNo"),
                ("Date", "TransactionDate"),
                ("Type", "PaymentType"),
                ("Method", "PaymentMethod"),
                ("Gross total", "DocumentTotals/GrossTotal"),
            ],
        },
    ],
    custom: Some(|doc, out| {
        if doc.root().ns.starts_with(NO) {
            out.kind = "SAF-T Financial (Norway)".into();
        } else if let Some(c) = value(doc, 0, "Header/AuditFileCountry") {
            out.kind = format!("SAF-T — OECD Standard Audit File for Tax ({c})");
        }
        ledger_checks(
            doc,
            out,
            "Line/DebitAmount/Amount",
            "Line/CreditAmount/Amount",
        );
    }),
};

/// Polish JPK (Jednolity Plik Kontrolny) files: header, subject, sales and purchase records.
pub static JPK: XmlSpec = XmlSpec {
    family: "SAF-T",
    kind: "JPK — Polish audit file",
    matches: |root, ns| {
        root == "JPK" && (ns.contains("jpk.mf.gov.pl") || ns.contains("crd.gov.pl/wzor"))
    },
    version: |doc| {
        let form = value(doc, 0, "Naglowek/KodFormularza")?;
        let variant = value(doc, 0, "Naglowek/WariantFormularza").unwrap_or_default();
        Some(format!("{form} (variant {variant})"))
    },
    facts: &[
        F("Header", "Form", "Naglowek/KodFormularza"),
        F(
            "Header",
            "Form version",
            "Naglowek/KodFormularza/@wersjaSchemy",
        ),
        F("Header", "Variant", "Naglowek/WariantFormularza"),
        F("Header", "Created", "Naglowek/DataWytworzeniaJPK"),
        F("Period", "From", "Naglowek/DataOd"),
        F("Period", "To", "Naglowek/DataDo"),
        F("Period", "Year", "Naglowek/Rok"),
        F("Period", "Month", "Naglowek/Miesiac"),
        F("Header", "Tax office", "Naglowek/KodUrzedu"),
        F("Header", "System", "Naglowek/NazwaSystemu"),
        F("Subject", "Tax ID (NIP)", "Podmiot1/**/NIP"),
        F("Subject", "Name", "Podmiot1/**/PelnaNazwa"),
        F("Subject", "E-mail", "Podmiot1/**/Email"),
        F(
            "Sales",
            "Rows (declared)",
            "**/SprzedazCtrl/LiczbaWierszySprzedazy",
        ),
        F(
            "Sales",
            "Tax due (declared)",
            "**/SprzedazCtrl/PodatekNalezny",
        ),
        F(
            "Purchases",
            "Rows (declared)",
            "**/ZakupCtrl/LiczbaWierszyZakupow",
        ),
        F(
            "Purchases",
            "Input tax (declared)",
            "**/ZakupCtrl/PodatekNaliczony",
        ),
    ],
    tables: &[
        T {
            name: "Sales records",
            rows: "**/SprzedazWiersz",
            columns: &[
                ("No.", "LpSprzedazy"),
                ("Counterparty tax ID", "NrKontrahenta"),
                ("Counterparty", "NazwaKontrahenta"),
                ("Document", "DowodSprzedazy"),
                ("Issued", "DataWystawienia"),
                ("Sold", "DataSprzedazy"),
                ("Net (K_19)", "K_19"),
                ("VAT (K_20)", "K_20"),
            ],
        },
        T {
            name: "Purchase records",
            rows: "**/ZakupWiersz",
            columns: &[
                ("No.", "LpZakupu"),
                ("Supplier tax ID", "NrDostawcy"),
                ("Supplier", "NazwaDostawcy"),
                ("Document", "DowodZakupu"),
                ("Date", "DataZakupu"),
                ("Net (K_42)", "K_42"),
                ("VAT (K_43)", "K_43"),
            ],
        },
    ],
    custom: Some(|doc, out| {
        compare(
            out,
            "Number of sales records",
            value(doc, 0, "**/SprzedazCtrl/LiczbaWierszySprzedazy"),
            Some(select(doc, 0, "**/SprzedazWiersz").len().to_string()),
        );
        compare(
            out,
            "Number of purchase records",
            value(doc, 0, "**/ZakupCtrl/LiczbaWierszyZakupow"),
            Some(select(doc, 0, "**/ZakupWiersz").len().to_string()),
        );
        out.notes.push(
            "Only the record counts are checked; the K_ field totals depend on the form version"
                .into(),
        );
    }),
};
