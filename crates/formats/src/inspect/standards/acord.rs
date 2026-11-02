//! ACORD XML insurance messages: Property & Casualty / Surety (root `ACORD`) and Life &
//! Annuity (root `TXLife`). The standards define thousands of aggregates; only the common ones
//! (policies, insured parties, coverages, claims and losses) are interpreted.
use super::support::{select_any, table, value};
use crate::inspect::{
    Inspection, Stage, State,
    spec::{F, T, XmlSpec},
    xmldoc::Doc,
};

fn acord_ns(ns: &str) -> bool {
    ns.is_empty() || ns.to_ascii_lowercase().contains("acord")
}

fn interpreted_in_part(out: &mut Inspection) {
    out.check(
        Stage::Parsed,
        State::Partial,
        "ACORD defines thousands of aggregates; only policies, parties, coverages and claims are interpreted",
    );
}

fn pc_custom(doc: &Doc, out: &mut Inspection) {
    let policies = select_any(doc, 0, &["**/PersPolicy", "**/CommlPolicy", "**/Policy"]);
    let columns: &[(&str, &str)] = &[
        ("Policy number", "PolicyNumber"),
        ("Line of business", "LOBCd"),
        ("Effective", "ContractTerm/EffectiveDt"),
        ("Expiration", "ContractTerm/ExpirationDt"),
        (
            "Premium",
            "CurrentTermAmt/Amt | FullTermAmt/Amt | WrittenAmt/Amt",
        ),
        ("Status", "PolicyStatusCd"),
        ("State", "ControllingStateProvCd"),
        ("Carrier", "NAICCd | CompanyProductCd"),
    ];
    if let Some(t) = table(doc, "Policies", &policies, columns) {
        out.tables.insert(0, t);
    }
    // The message: which request or response, from the element under InsuranceSvcRq/Rs.
    for svc in select_any(doc, 0, &["InsuranceSvcRq", "InsuranceSvcRs"]) {
        if let Some(&m) = doc
            .get(svc)
            .children
            .iter()
            .find(|&&c| doc.get(c).name.ends_with("Rq") || doc.get(c).name.ends_with("Rs"))
        {
            out.fact(
                "Message",
                "Message",
                doc.get(m).name.clone(),
                Some(doc.get(m).span.clone()),
            );
            break;
        }
    }
    let vehicles = select_any(doc, 0, &["**/PersVeh", "**/CommlVeh"]).len();
    if vehicles > 0 {
        out.fact("Counts", "Vehicles", vehicles.to_string(), None);
    }
    interpreted_in_part(out);
}

pub static PC: XmlSpec = XmlSpec {
    family: "ACORD",
    kind: "ACORD XML (Property & Casualty / Surety)",
    matches: |root, ns| root == "ACORD" && acord_ns(ns),
    version: |doc| value(doc, 0, "@version"),
    facts: &[
        F(
            "Message",
            "Request ID",
            "**/InsuranceSvcRq/RqUID | **/InsuranceSvcRs/RqUID",
        ),
        F(
            "Message",
            "Transaction date",
            "**/TransactionRequestDt | **/TransactionResponseDt",
        ),
        F(
            "Message",
            "Sender",
            "**/SignonRq/ClientApp/Org | **/SignonRs/ServerDt",
        ),
        F(
            "Message",
            "Status",
            "**/MsgStatus/MsgStatusCd | **/Status/StatusCd",
        ),
        F("Message", "Currency", "**/CurCd"),
        F("Policy", "Policy number", "**/PolicyNumber"),
        F("Policy", "Line of business", "**/LOBCd"),
        F("Policy", "Effective", "**/ContractTerm/EffectiveDt"),
        F("Policy", "Expiration", "**/ContractTerm/ExpirationDt"),
        F(
            "Policy",
            "Premium",
            "**/PersPolicy/CurrentTermAmt/Amt | **/CommlPolicy/CurrentTermAmt/Amt | **/Policy/CurrentTermAmt/Amt",
        ),
        F(
            "Producer",
            "Name",
            "**/Producer/GeneralPartyInfo/NameInfo/CommlName/CommercialName",
        ),
        F(
            "Producer",
            "Contract number",
            "**/Producer/ProducerInfo/ContractNumber",
        ),
        F("Counts", "Insured parties", "count:**/InsuredOrPrincipal"),
        F("Counts", "Coverages", "count:**/Coverage"),
        F("Counts", "Claims", "count:**/ClaimsOccurrence"),
        F("Counts", "Prior losses", "count:**/Loss"),
        F("Counts", "Locations", "count:**/Location"),
    ],
    tables: &[
        T {
            name: "Insured parties",
            rows: "**/InsuredOrPrincipal",
            columns: &[
                (
                    "Name",
                    "GeneralPartyInfo/NameInfo/CommlName/CommercialName | GeneralPartyInfo/NameInfo/PersonName/Surname",
                ),
                (
                    "Given name",
                    "GeneralPartyInfo/NameInfo/PersonName/GivenName",
                ),
                ("Role", "InsuredOrPrincipalInfo/InsuredOrPrincipalRoleCd"),
                ("Street", "GeneralPartyInfo/Addr/Addr1"),
                ("City", "GeneralPartyInfo/Addr/City"),
                ("State", "GeneralPartyInfo/Addr/StateProvCd"),
                ("Postal code", "GeneralPartyInfo/Addr/PostalCode"),
                ("Tax ID", "GeneralPartyInfo/NameInfo/TaxIdentity/TaxId"),
                ("Birth date", "InsuredOrPrincipalInfo/PersonInfo/BirthDt"),
            ],
        },
        T {
            name: "Coverages",
            rows: "**/Coverage",
            columns: &[
                ("Code", "CoverageCd"),
                ("Description", "CoverageDesc"),
                ("Limit", "Limit/FormatCurrencyAmt/Amt | Limit/FormatInteger"),
                ("Limit applies to", "Limit/LimitAppliesToCd"),
                (
                    "Deductible",
                    "Deductible/FormatCurrencyAmt/Amt | Deductible/FormatInteger",
                ),
                ("Deductible type", "Deductible/DeductibleTypeCd"),
                ("Premium", "CurrentTermAmt/Amt"),
            ],
        },
        T {
            name: "Claims",
            rows: "**/ClaimsOccurrence",
            columns: &[
                (
                    "Claim number",
                    "ItemIdInfo/InsurerId | ItemIdInfo/AgencyId | @id",
                ),
                ("Loss date", "LossDt"),
                ("Loss time", "LossTime"),
                ("Reported", "ClaimsReported/ReportedDt"),
                ("Status", "ClaimStatusCd | ClaimsStatusCd"),
                ("Cause", "LossCauseCd"),
                ("Description", "IncidentDesc | ClaimDesc"),
                (
                    "Amount",
                    "**/ClaimsPayment/TotalPaymentAmt/Amt | **/ReserveAmt/Amt | **/LossAmt/Amt",
                ),
            ],
        },
        T {
            name: "Prior losses",
            rows: "**/Loss",
            columns: &[
                ("Loss date", "LossDt"),
                ("Cause", "LossCauseCd"),
                ("Claim status", "ClaimStatusCd"),
                ("Paid", "TotalPaidAmt/Amt"),
                ("Description", "LossDesc"),
            ],
        },
    ],
    custom: Some(pc_custom),
};

pub static LIFE: XmlSpec = XmlSpec {
    family: "ACORD",
    kind: "ACORD XML (Life & Annuity, TXLife)",
    matches: |root, ns| root == "TXLife" && acord_ns(ns),
    version: |doc| value(doc, 0, "@Version | **/TXLifeRequest/@Version"),
    facts: &[
        F(
            "Message",
            "Transaction ID",
            "**/TXLifeRequest/TransRefGUID | **/TXLifeResponse/TransRefGUID",
        ),
        F("Message", "Transaction type", "**/TransType"),
        F("Message", "Transaction date", "**/TransExeDate"),
        F("Message", "Result", "**/TransResult/ResultCode"),
        F("Policy", "Policy number", "**/Holding/Policy/PolNumber"),
        F(
            "Policy",
            "Line of business",
            "**/Holding/Policy/LineOfBusiness",
        ),
        F(
            "Policy",
            "Product",
            "**/Holding/Policy/ProductCode | **/Holding/Policy/PlanName",
        ),
        F("Policy", "Carrier", "**/Holding/Policy/CarrierCode"),
        F("Policy", "Status", "**/Holding/Policy/PolicyStatus"),
        F(
            "Policy",
            "Effective",
            "**/Holding/Policy/EffDate | **/Holding/Policy/IssueDate",
        ),
        F("Policy", "Termination", "**/Holding/Policy/TermDate"),
        F("Policy", "Premium", "**/Holding/Policy/PaymentAmt"),
        F("Policy", "Payment mode", "**/Holding/Policy/PaymentMode"),
        F("Policy", "Face amount", "**/Holding/Policy/Life/FaceAmt"),
        F("Counts", "Parties", "count:**/OLifE/Party"),
        F("Counts", "Coverages", "count:**/Coverage"),
        F("Counts", "Claims", "count:**/Claim"),
    ],
    tables: &[
        T {
            name: "Policies",
            rows: "**/Holding/Policy",
            columns: &[
                ("Policy number", "PolNumber"),
                ("Line of business", "LineOfBusiness"),
                ("Product", "ProductCode | PlanName"),
                ("Status", "PolicyStatus"),
                ("Effective", "EffDate | IssueDate"),
                ("Termination", "TermDate"),
                ("Premium", "PaymentAmt"),
                ("Face amount", "Life/FaceAmt"),
            ],
        },
        T {
            name: "Parties",
            rows: "**/OLifE/Party",
            columns: &[
                ("ID", "@id"),
                (
                    "Name",
                    "FullName | Organization/DTCCMemberCode | Organization/OrgCode",
                ),
                ("First name", "Person/FirstName"),
                ("Last name", "Person/LastName"),
                ("Birth date", "Person/BirthDate"),
                ("Gender", "Person/Gender"),
                ("Government ID", "GovtID"),
            ],
        },
        T {
            name: "Coverages",
            rows: "**/Coverage",
            columns: &[
                ("ID", "@id"),
                ("Plan", "PlanName | ProductCode"),
                ("Type", "LifeCovTypeCode"),
                ("Amount", "CurrentAmt | InitCovAmt"),
                ("Effective", "EffDate"),
                ("Status", "LifeCovStatus"),
            ],
        },
        T {
            name: "Relations",
            rows: "**/OLifE/Relation",
            columns: &[
                ("From", "@OriginatingObjectID | OriginatingObjectID"),
                ("To", "@RelatedObjectID | RelatedObjectID"),
                ("Role", "RelationRoleCode"),
            ],
        },
        T {
            name: "Claims",
            rows: "**/Claim",
            columns: &[
                ("Claim number", "ClaimNumber | @id"),
                ("Loss date", "DateOfLoss | IncidentDate"),
                ("Status", "ClaimStatus"),
                ("Amount", "ClaimAmt | TotalClaimAmt"),
            ],
        },
    ],
    custom: Some(|_, out| interpreted_in_part(out)),
};
