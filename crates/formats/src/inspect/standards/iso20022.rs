//! ISO 20022 financial messages, recognized by their namespace
//! (`urn:iso:std:iso:20022:tech:xsd:<area>.<message>.<variant>.<version>`).
use crate::inspect::{
    Inspection, Stage, State,
    spec::{F, T, XmlSpec, compare_total, decimal_sum, select, value_at},
    xmldoc::Doc,
};

const PREFIX: &str = "urn:iso:std:iso:20022:tech:xsd:";

/// The message identifier from the namespace: "camt.053.001.08".
fn message(ns: &str) -> Option<&str> {
    ns.strip_prefix(PREFIX)
}
fn is(ns: &str, id: &str) -> bool {
    message(ns).is_some_and(|m| m.starts_with(id))
}
/// "camt.053.001.08" -> "001.08" (variant and version).
fn version(doc: &Doc) -> Option<String> {
    let m = message(&doc.root().ns)?;
    let parts: Vec<&str> = m.split('.').collect();
    (parts.len() == 4).then(|| format!("{} (variant {}, version {})", m, parts[2], parts[3]))
}

/// Party name, account (IBAN or other) and agent BIC, under a party element `p`.
macro_rules! party {
    ($group:literal, $p:literal) => {
        [
            F($group, "Name", concat!("**/", $p, "/Nm")),
            F(
                $group,
                "Account",
                concat!("**/", $p, "Acct/Id/IBAN | **/", $p, "Acct/Id/Othr/Id"),
            ),
            F(
                $group,
                "Agent (BIC)",
                concat!(
                    "**/",
                    $p,
                    "Agt/FinInstnId/BICFI | **/",
                    $p,
                    "Agt/FinInstnId/BIC"
                ),
            ),
        ]
    };
}

const DEBTOR: [F; 3] = party!("Debtor", "Dbtr");

pub static PAIN_001: XmlSpec = XmlSpec {
    family: "ISO 20022",
    kind: "pain.001 — customer credit transfer initiation",
    matches: |root, ns| root == "Document" && is(ns, "pain.001."),
    version,
    facts: &[
        F("Message", "Message ID", "**/GrpHdr/MsgId"),
        F("Message", "Created", "**/GrpHdr/CreDtTm"),
        F("Message", "Initiating party", "**/GrpHdr/InitgPty/Nm"),
        F("Message", "Transactions (declared)", "**/GrpHdr/NbOfTxs"),
        F("Message", "Control sum (declared)", "**/GrpHdr/CtrlSum"),
        F("Payment", "Payment information ID", "**/PmtInf/PmtInfId"),
        F("Payment", "Method", "**/PmtInf/PmtMtd"),
        F(
            "Payment",
            "Requested execution date",
            "**/PmtInf/ReqdExctnDt/Dt | **/PmtInf/ReqdExctnDt/DtTm | **/PmtInf/ReqdExctnDt",
        ),
        DEBTOR[0],
        DEBTOR[1],
        DEBTOR[2],
    ],
    tables: &[T {
        name: "Credit transfers",
        rows: "**/CdtTrfTxInf",
        columns: &[
            ("End-to-end ID", "PmtId/EndToEndId"),
            ("Instruction ID", "PmtId/InstrId"),
            ("Amount", "Amt/InstdAmt"),
            ("Currency", "Amt/InstdAmt/@Ccy"),
            ("Creditor", "Cdtr/Nm"),
            ("Creditor account", "CdtrAcct/Id/IBAN | CdtrAcct/Id/Othr/Id"),
            (
                "Creditor agent",
                "CdtrAgt/FinInstnId/BICFI | CdtrAgt/FinInstnId/BIC",
            ),
            ("Remittance", "RmtInf/Ustrd | RmtInf/Strd/CdtrRefInf/Ref"),
            ("Debtor", "../Dbtr/Nm"),
            ("Execution date", "../ReqdExctnDt/Dt | ../ReqdExctnDt"),
        ],
    }],
    custom: Some(|doc, out| {
        let declared_n = value_at(doc, 0, "**/GrpHdr/NbOfTxs").map(|v| v.0);
        let count = select(doc, 0, "**/CdtTrfTxInf").len().to_string();
        compare_total(out, "Number of transactions", declared_n, Some(count));
        let declared = value_at(doc, 0, "**/GrpHdr/CtrlSum").map(|v| v.0);
        let amounts: Vec<String> = select(doc, 0, "**/CdtTrfTxInf/Amt/InstdAmt")
            .iter()
            .map(|&e| doc.text(e))
            .collect();
        compare_total(
            out,
            "Control sum",
            declared,
            decimal_sum(amounts.iter().map(String::as_str)),
        );
    }),
};

pub static PAIN_002: XmlSpec = XmlSpec {
    family: "ISO 20022",
    kind: "pain.002 — customer payment status report",
    matches: |root, ns| root == "Document" && is(ns, "pain.002."),
    version,
    facts: &[
        F("Message", "Message ID", "**/GrpHdr/MsgId"),
        F("Message", "Created", "**/GrpHdr/CreDtTm"),
        F(
            "Original message",
            "Message ID",
            "**/OrgnlGrpInfAndSts/OrgnlMsgId",
        ),
        F(
            "Original message",
            "Message type",
            "**/OrgnlGrpInfAndSts/OrgnlMsgNmId",
        ),
        F(
            "Original message",
            "Group status",
            "**/OrgnlGrpInfAndSts/GrpSts",
        ),
        F(
            "Original message",
            "Group status reason",
            "**/OrgnlGrpInfAndSts/StsRsnInf/Rsn/Cd",
        ),
        F(
            "Original message",
            "Transactions",
            "**/OrgnlGrpInfAndSts/OrgnlNbOfTxs",
        ),
    ],
    tables: &[
        T {
            name: "Transaction statuses",
            rows: "**/TxInfAndSts",
            columns: &[
                ("Original end-to-end ID", "OrgnlEndToEndId"),
                ("Original instruction ID", "OrgnlInstrId"),
                ("Status", "TxSts"),
                ("Reason code", "StsRsnInf/Rsn/Cd | StsRsnInf/Rsn/Prtry"),
                ("Additional information", "StsRsnInf/AddtlInf"),
                ("Amount", "OrgnlTxRef/Amt/InstdAmt"),
                ("Currency", "OrgnlTxRef/Amt/InstdAmt/@Ccy"),
                ("Original payment information", "../OrgnlPmtInfId"),
            ],
        },
        T {
            name: "Payment information statuses",
            rows: "**/OrgnlPmtInfAndSts",
            columns: &[
                ("Original payment information ID", "OrgnlPmtInfId"),
                ("Status", "PmtInfSts"),
                ("Reason code", "StsRsnInf/Rsn/Cd"),
                ("Transactions", "OrgnlNbOfTxs"),
            ],
        },
    ],
    custom: Some(|_, out| {
        out.notes.push(
            "Status codes are ISO 20022 external codes (for example ACCP accepted, ACSC settled, RJCT rejected)"
                .into(),
        );
    }),
};

pub static PACS_008: XmlSpec = XmlSpec {
    family: "ISO 20022",
    kind: "pacs.008 — FI to FI customer credit transfer",
    matches: |root, ns| root == "Document" && is(ns, "pacs.008."),
    version,
    facts: &[
        F("Message", "Message ID", "**/GrpHdr/MsgId"),
        F("Message", "Created", "**/GrpHdr/CreDtTm"),
        F("Message", "Transactions", "**/GrpHdr/NbOfTxs"),
        F("Settlement", "Method", "**/GrpHdr/SttlmInf/SttlmMtd"),
        F(
            "Settlement",
            "Interbank settlement date",
            "**/GrpHdr/IntrBkSttlmDt | **/CdtTrfTxInf/IntrBkSttlmDt",
        ),
        F("Settlement", "Total amount", "**/GrpHdr/TtlIntrBkSttlmAmt"),
        F("Settlement", "Currency", "**/GrpHdr/TtlIntrBkSttlmAmt/@Ccy"),
        F(
            "Agents",
            "Instructing agent",
            "**/InstgAgt/FinInstnId/BICFI | **/InstgAgt/FinInstnId/BIC",
        ),
        F(
            "Agents",
            "Instructed agent",
            "**/InstdAgt/FinInstnId/BICFI | **/InstdAgt/FinInstnId/BIC",
        ),
    ],
    tables: &[T {
        name: "Credit transfers",
        rows: "**/CdtTrfTxInf",
        columns: &[
            ("End-to-end ID", "PmtId/EndToEndId"),
            ("Transaction ID", "PmtId/TxId"),
            ("UETR", "PmtId/UETR"),
            ("Settlement amount", "IntrBkSttlmAmt"),
            ("Currency", "IntrBkSttlmAmt/@Ccy"),
            ("Charges", "ChrgBr"),
            ("Debtor", "Dbtr/Nm"),
            ("Debtor account", "DbtrAcct/Id/IBAN | DbtrAcct/Id/Othr/Id"),
            (
                "Debtor agent",
                "DbtrAgt/FinInstnId/BICFI | DbtrAgt/FinInstnId/BIC",
            ),
            ("Creditor", "Cdtr/Nm"),
            ("Creditor account", "CdtrAcct/Id/IBAN | CdtrAcct/Id/Othr/Id"),
            (
                "Creditor agent",
                "CdtrAgt/FinInstnId/BICFI | CdtrAgt/FinInstnId/BIC",
            ),
            ("Remittance", "RmtInf/Ustrd"),
        ],
    }],
    custom: Some(|doc, out| {
        let declared = value_at(doc, 0, "**/GrpHdr/TtlIntrBkSttlmAmt").map(|v| v.0);
        let amounts: Vec<String> = select(doc, 0, "**/CdtTrfTxInf/IntrBkSttlmAmt")
            .iter()
            .map(|&e| doc.text(e))
            .collect();
        compare_total(
            out,
            "Total settlement amount",
            declared,
            decimal_sum(amounts.iter().map(String::as_str)),
        );
    }),
};

const STATEMENT_ENTRIES: T = T {
    name: "Entries",
    rows: "**/Ntry",
    columns: &[
        ("Booking date", "BookgDt/Dt | BookgDt/DtTm"),
        ("Value date", "ValDt/Dt | ValDt/DtTm"),
        ("Credit/debit", "CdtDbtInd"),
        ("Amount", "Amt"),
        ("Currency", "Amt/@Ccy"),
        ("Status", "Sts/Cd | Sts"),
        ("Bank reference", "AcctSvcrRef"),
        ("End-to-end ID", "NtryDtls/TxDtls/Refs/EndToEndId"),
        (
            "Counterparty",
            "NtryDtls/TxDtls/RltdPties/Cdtr/Nm | NtryDtls/TxDtls/RltdPties/Cdtr/Pty/Nm | NtryDtls/TxDtls/RltdPties/Dbtr/Nm | NtryDtls/TxDtls/RltdPties/Dbtr/Pty/Nm",
        ),
        ("Remittance", "NtryDtls/TxDtls/RmtInf/Ustrd | AddtlNtryInf"),
        ("Bank transaction code", "BkTxCd/Domn/Cd | BkTxCd/Prtry/Cd"),
    ],
};
const BALANCES: T = T {
    name: "Balances",
    rows: "**/Bal",
    columns: &[
        ("Type", "Tp/CdOrPrtry/Cd | Tp/CdOrPrtry/Prtry"),
        ("Amount", "Amt"),
        ("Currency", "Amt/@Ccy"),
        ("Credit/debit", "CdtDbtInd"),
        ("Date", "Dt/Dt | Dt/DtTm"),
    ],
};
const ACCOUNT_FACTS: [F; 5] = [
    F("Account", "IBAN", "**/Acct/Id/IBAN | **/Acct/Id/Othr/Id"),
    F("Account", "Currency", "**/Acct/Ccy"),
    F("Account", "Owner", "**/Acct/Ownr/Nm"),
    F(
        "Account",
        "Servicer (BIC)",
        "**/Acct/Svcr/FinInstnId/BICFI | **/Acct/Svcr/FinInstnId/BIC",
    ),
    F("Message", "Message ID", "**/GrpHdr/MsgId"),
];

/// Opening balance + credits − debits = closing balance (computed), for statements.
fn balance_check(doc: &Doc, out: &mut Inspection, opening: &str, closing: &str) {
    let signed = |bal: &str| -> Option<String> {
        let amt = value_at(doc, 0, &format!("**/Bal[Tp/CdOrPrtry/Cd={bal}]/Amt"))?.0;
        let ind = value_at(doc, 0, &format!("**/Bal[Tp/CdOrPrtry/Cd={bal}]/CdtDbtInd"))
            .map(|v| v.0)
            .unwrap_or_default();
        Some(if ind == "DBIT" {
            format!("-{amt}")
        } else {
            amt
        })
    };
    let (Some(open), Some(close)) = (signed(opening), signed(closing)) else {
        return;
    };
    let mut values = vec![open.clone()];
    for e in select(doc, 0, "**/Ntry") {
        let amt = value_at(doc, e, "Amt").map(|v| v.0).unwrap_or_default();
        let ind = value_at(doc, e, "CdtDbtInd")
            .map(|v| v.0)
            .unwrap_or_default();
        values.push(if ind == "DBIT" {
            format!("-{amt}")
        } else {
            amt
        });
    }
    let computed = decimal_sum(values.iter().map(String::as_str));
    out.fact("Balances", &format!("Opening ({opening})"), open, None);
    out.fact(
        "Balances",
        &format!("Closing ({closing})"),
        close.clone(),
        None,
    );
    compare_total(
        out,
        "Closing balance (opening + credits − debits)",
        Some(close),
        computed,
    );
}

pub static CAMT_053: XmlSpec = XmlSpec {
    family: "ISO 20022",
    kind: "camt.053 — bank-to-customer statement",
    matches: |root, ns| root == "Document" && is(ns, "camt.053."),
    version,
    facts: &[
        ACCOUNT_FACTS[4],
        F("Message", "Created", "**/GrpHdr/CreDtTm"),
        F("Statement", "Statement ID", "**/Stmt/Id"),
        F("Statement", "Electronic sequence", "**/Stmt/ElctrncSeqNb"),
        F("Statement", "From", "**/Stmt/FrToDt/FrDtTm"),
        F("Statement", "To", "**/Stmt/FrToDt/ToDtTm"),
        F("Statement", "Entries", "count:**/Ntry"),
        ACCOUNT_FACTS[0],
        ACCOUNT_FACTS[1],
        ACCOUNT_FACTS[2],
        ACCOUNT_FACTS[3],
    ],
    tables: &[STATEMENT_ENTRIES, BALANCES],
    custom: Some(|doc, out| balance_check(doc, out, "OPBD", "CLBD")),
};

pub static CAMT_052: XmlSpec = XmlSpec {
    family: "ISO 20022",
    kind: "camt.052 — bank-to-customer account report (intraday)",
    matches: |root, ns| root == "Document" && is(ns, "camt.052."),
    version,
    facts: &[
        ACCOUNT_FACTS[4],
        F("Message", "Created", "**/GrpHdr/CreDtTm"),
        F("Report", "Report ID", "**/Rpt/Id"),
        F("Report", "From", "**/Rpt/FrToDt/FrDtTm"),
        F("Report", "To", "**/Rpt/FrToDt/ToDtTm"),
        F("Report", "Entries", "count:**/Ntry"),
        ACCOUNT_FACTS[0],
        ACCOUNT_FACTS[1],
        ACCOUNT_FACTS[2],
        ACCOUNT_FACTS[3],
    ],
    tables: &[STATEMENT_ENTRIES, BALANCES],
    custom: None,
};

pub static CAMT_054: XmlSpec = XmlSpec {
    family: "ISO 20022",
    kind: "camt.054 — debit/credit notification",
    matches: |root, ns| root == "Document" && is(ns, "camt.054."),
    version,
    facts: &[
        ACCOUNT_FACTS[4],
        F("Message", "Created", "**/GrpHdr/CreDtTm"),
        F("Notification", "Notification ID", "**/Ntfctn/Id"),
        F("Notification", "Entries", "count:**/Ntry"),
        ACCOUNT_FACTS[0],
        ACCOUNT_FACTS[1],
        ACCOUNT_FACTS[2],
    ],
    tables: &[STATEMENT_ENTRIES],
    custom: None,
};

/// Any other ISO 20022 message: its identity and header.
pub static OTHER: XmlSpec = XmlSpec {
    family: "ISO 20022",
    kind: "ISO 20022 message",
    matches: |root, ns| root == "Document" && message(ns).is_some(),
    version,
    facts: &[
        F(
            "Message",
            "Message ID",
            "**/GrpHdr/MsgId | **/Assgnmt/Id | **/Hdr/MsgId",
        ),
        F(
            "Message",
            "Created",
            "**/GrpHdr/CreDtTm | **/Assgnmt/CreDtTm",
        ),
        F("Message", "Transactions", "**/GrpHdr/NbOfTxs"),
    ],
    tables: &[],
    custom: Some(|doc, out| {
        if let Some(m) = message(&doc.root().ns) {
            let id = m.split('.').take(2).collect::<Vec<_>>().join(".");
            out.kind = format!("{id} — ISO 20022 message (no specific view yet)");
            out.check(
                Stage::Parsed,
                State::Partial,
                "Only the header is interpreted for this message type",
            );
        }
    }),
};
