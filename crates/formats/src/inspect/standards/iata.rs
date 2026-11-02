//! IATA Cargo-XML messages (`iata:<message>:1`): air waybills (XFWB), house waybills (XFZB),
//! flight manifests (XFFM) and the header of the others. Air waybill numbers are checked with
//! their mod-7 check digit; declared piece totals are compared with the items.
use super::support::{compare, sum_at, value};
use crate::inspect::{
    Inspection, Stage, State,
    spec::{F, T, XmlSpec, select},
    xmldoc::Doc,
};

fn iata(ns: &str) -> Option<&str> {
    ns.strip_prefix("iata:")
        .map(|r| r.split(':').next().unwrap_or(r))
}
fn version(doc: &Doc) -> Option<String> {
    let schema = doc.root().ns.rsplit(':').next().map(str::to_owned);
    let v = value(doc, 0, "MessageHeaderDocument/VersionID");
    match (v, schema) {
        (Some(v), Some(s)) => Some(format!("{v} (schema {s})")),
        (v, s) => v.or(s),
    }
}

/// An air waybill number "176-12345675": the serial's last digit is the first seven mod 7.
pub fn awb_ok(awb: &str) -> bool {
    let digits: String = awb.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 11
        || awb
            .chars()
            .any(|c| !(c.is_ascii_digit() || c == '-' || c == ' '))
    {
        return false;
    }
    let serial = &digits[3..10];
    let check = digits[10..].parse::<u32>().ok();
    serial.parse::<u32>().ok().map(|s| s % 7) == check
}

fn awb_check(out: &mut Inspection, numbers: &[String]) {
    let numbers: Vec<&String> = numbers
        .iter()
        .filter(|n| n.chars().filter(|c| c.is_ascii_digit()).count() == 11)
        .collect();
    if numbers.is_empty() {
        return;
    }
    let bad: Vec<&str> = numbers
        .iter()
        .filter(|n| !awb_ok(n))
        .map(|n| n.as_str())
        .collect();
    out.check(
        Stage::Semantic,
        if bad.is_empty() {
            State::Passed
        } else {
            State::Failed
        },
        if bad.is_empty() {
            format!(
                "Air waybill check digits (mod 7) of {} numbers are valid",
                numbers.len()
            )
        } else {
            format!(
                "Air waybill check digit (mod 7) is wrong for {}",
                bad.iter().take(5).copied().collect::<Vec<_>>().join(", ")
            )
        },
    );
}

const HEADER: [F; 6] = [
    F("Message", "ID", "MessageHeaderDocument/ID"),
    F("Message", "Name", "MessageHeaderDocument/Name"),
    F("Message", "Type code", "MessageHeaderDocument/TypeCode"),
    F("Message", "Issued", "MessageHeaderDocument/IssueDateTime"),
    F(
        "Message",
        "Sender",
        "MessageHeaderDocument/SenderParty/PrimaryID",
    ),
    F(
        "Message",
        "Recipient",
        "MessageHeaderDocument/RecipientParty/PrimaryID",
    ),
];

macro_rules! party {
    ($group:literal, $p:literal) => {
        [
            F($group, "Name", concat!("**/", $p, "/Name")),
            F($group, "Account", concat!("**/", $p, "/AccountID")),
            F(
                $group,
                "Street",
                concat!("**/", $p, "/PostalStructuredAddress/StreetName"),
            ),
            F(
                $group,
                "City",
                concat!("**/", $p, "/PostalStructuredAddress/CityName"),
            ),
            F(
                $group,
                "Country",
                concat!("**/", $p, "/PostalStructuredAddress/CountryID"),
            ),
        ]
    };
}
const SHIPPER: [F; 5] = party!("Shipper", "ConsignorParty");
const CONSIGNEE: [F; 5] = party!("Consignee", "ConsigneeParty");

pub static WAYBILL: XmlSpec = XmlSpec {
    family: "IATA Cargo-XML",
    kind: "Air waybill (XFWB)",
    matches: |root, ns| {
        (root == "Waybill" && iata(ns) == Some("waybill"))
            || (root == "HouseWaybill" && iata(ns) == Some("housewaybill"))
    },
    version,
    facts: &[
        HEADER[0],
        HEADER[1],
        HEADER[2],
        HEADER[3],
        HEADER[4],
        HEADER[5],
        F("Waybill", "Number", "BusinessHeaderDocument/ID"),
        F(
            "Waybill",
            "Master waybill",
            "**/TransportContractDocument/ID",
        ),
        F(
            "Waybill",
            "Issued at",
            "**/SignatoryCarrierAuthentication/IssueAuthenticationLocation/Name",
        ),
        F(
            "Waybill",
            "Issued on",
            "**/SignatoryCarrierAuthentication/ActualDateTime",
        ),
        F(
            "Waybill",
            "Carrier signatory",
            "**/SignatoryCarrierAuthentication/Signatory",
        ),
        F("Route", "Origin", "**/OriginLocation/ID"),
        F("Route", "Origin name", "**/OriginLocation/Name"),
        F("Route", "Destination", "**/FinalDestinationLocation/ID"),
        F(
            "Route",
            "Destination name",
            "**/FinalDestinationLocation/Name",
        ),
        SHIPPER[0],
        SHIPPER[1],
        SHIPPER[2],
        SHIPPER[3],
        SHIPPER[4],
        CONSIGNEE[0],
        CONSIGNEE[1],
        CONSIGNEE[2],
        CONSIGNEE[3],
        CONSIGNEE[4],
        F("Forwarder", "Name", "**/FreightForwarderParty/Name"),
        F("Cargo", "Pieces", "**/TotalPieceQuantity"),
        F("Cargo", "Gross weight", "**/IncludedTareGrossWeightMeasure"),
        F(
            "Cargo",
            "Weight unit",
            "**/IncludedTareGrossWeightMeasure/@unitCode",
        ),
        F("Cargo", "Volume", "**/GrossVolumeMeasure"),
        F("Cargo", "Volume unit", "**/GrossVolumeMeasure/@unitCode"),
        F(
            "Cargo",
            "Goods",
            "**/SummaryDescription | **/NatureIdentificationTransportCargo/Identification",
        ),
        F(
            "Cargo",
            "Special handling",
            "**/HandlingSPHInstructions/DescriptionCode",
        ),
        F(
            "Charges",
            "Total",
            "**/ApplicableTotalRating/ApplicablePrepaidCollectMonetarySummation/GrandTotalAmount",
        ),
        F(
            "Charges",
            "Currency",
            "**/ApplicableTotalRating/TypeCode | **/ApplicableTotalRating/ApplicablePrepaidCollectMonetarySummation/GrandTotalAmount/@currencyID",
        ),
    ],
    tables: &[
        T {
            name: "Routing",
            rows: "**/SpecifiedLogisticsTransportMovement",
            columns: &[
                ("Sequence", "SequenceNumeric"),
                ("Flight", "ID"),
                ("Carrier", "UsedLogisticsTransportMeans/Name"),
                ("From", "DepartureEvent/OccurrenceDepartureLocation/ID"),
                ("Departure", "DepartureEvent/ScheduledOccurrenceDateTime"),
                ("To", "ArrivalEvent/OccurrenceArrivalLocation/ID"),
                ("Arrival", "ArrivalEvent/ScheduledOccurrenceDateTime"),
                ("Mode", "ModeCode"),
            ],
        },
        T {
            name: "Items",
            rows: "**/IncludedMasterConsignmentItem",
            columns: &[
                ("Sequence", "SequenceNumeric"),
                ("Pieces", "PieceQuantity"),
                ("Gross weight", "GrossWeightMeasure"),
                ("Unit", "GrossWeightMeasure/@unitCode"),
                ("Volume", "GrossVolumeMeasure"),
                (
                    "Goods",
                    "NatureIdentificationTransportCargo/Identification | TypeCode",
                ),
                (
                    "Rate class",
                    "ApplicableFreightRateServiceCharge/CategoryCode",
                ),
                ("Charge", "ApplicableFreightRateServiceCharge/AppliedAmount"),
            ],
        },
        T {
            name: "House consignments",
            rows: "**/IncludedHouseConsignment",
            columns: &[
                ("Pieces", "TotalPieceQuantity"),
                ("Gross weight", "IncludedTareGrossWeightMeasure"),
                ("Unit", "IncludedTareGrossWeightMeasure/@unitCode"),
                ("Origin", "OriginLocation/ID"),
                ("Destination", "FinalDestinationLocation/ID"),
                ("Shipper", "ConsignorParty/Name"),
                ("Consignee", "ConsigneeParty/Name"),
                ("Goods", "SummaryDescription"),
            ],
        },
    ],
    custom: Some(|doc, out| {
        if doc.root().name == "HouseWaybill" {
            out.kind = "House waybill (XFZB)".into();
        }
        let mut numbers = vec![];
        if doc.root().name == "Waybill" {
            numbers.extend(value(doc, 0, "BusinessHeaderDocument/ID"));
        }
        numbers.extend(
            select(doc, 0, "**/TransportContractDocument/ID")
                .iter()
                .map(|&e| doc.text(e)),
        );
        awb_check(out, &numbers);
        compare(
            out,
            "Total pieces = sum of item pieces",
            value(doc, 0, "MasterConsignment/TotalPieceQuantity"),
            sum_at(
                doc,
                0,
                "MasterConsignment/IncludedMasterConsignmentItem/PieceQuantity",
            ),
        );
    }),
};

pub static FLIGHT_MANIFEST: XmlSpec = XmlSpec {
    family: "IATA Cargo-XML",
    kind: "Flight manifest (XFFM)",
    matches: |root, ns| root == "FlightManifest" && iata(ns) == Some("flightmanifest"),
    version,
    facts: &[
        HEADER[0],
        HEADER[1],
        HEADER[2],
        HEADER[3],
        HEADER[4],
        HEADER[5],
        F("Flight", "Manifest ID", "BusinessHeaderDocument/ID"),
        F("Flight", "Flight", "LogisticsTransportMovement/ID"),
        F(
            "Flight",
            "Carrier",
            "LogisticsTransportMovement/UsedLogisticsTransportMeans/Name",
        ),
        F(
            "Flight",
            "Aircraft",
            "LogisticsTransportMovement/UsedLogisticsTransportMeans/Type",
        ),
        F(
            "Flight",
            "Departure airport",
            "LogisticsTransportMovement/DepartureEvent/OccurrenceDepartureLocation/ID",
        ),
        F(
            "Flight",
            "Departure",
            "LogisticsTransportMovement/DepartureEvent/DepartureOccurrenceDateTime",
        ),
        F(
            "Flight",
            "Total pieces",
            "LogisticsTransportMovement/TotalPieceQuantity",
        ),
        F(
            "Flight",
            "Total gross weight",
            "LogisticsTransportMovement/TotalGrossWeightMeasure",
        ),
        F("Flight", "Arrival points", "count:ArrivalEvent"),
        F(
            "Flight",
            "Consignments",
            "count:**/IncludedMasterConsignment",
        ),
        F(
            "Flight",
            "Unit load devices",
            "count:**/UtilizedUnitLoadTransportEquipment",
        ),
    ],
    tables: &[
        T {
            name: "Legs",
            rows: "ArrivalEvent",
            columns: &[
                ("Arrival airport", "OccurrenceArrivalLocation/ID"),
                ("Airport name", "OccurrenceArrivalLocation/Name"),
                (
                    "Arrival",
                    "ArrivalOccurrenceDateTime | ScheduledOccurrenceDateTime",
                ),
                ("Departure", "DepartureEvent/DepartureOccurrenceDateTime"),
                ("Cargo units", "count:AssociatedTransportCargo"),
                ("Consignments", "count:**/IncludedMasterConsignment"),
            ],
        },
        T {
            name: "Consignments",
            rows: "**/IncludedMasterConsignment",
            columns: &[
                ("Air waybill", "TransportContractDocument/ID"),
                ("Origin", "OriginLocation/ID"),
                ("Destination", "FinalDestinationLocation/ID"),
                ("Pieces", "PieceQuantity"),
                ("Total pieces", "TotalPieceQuantity"),
                ("Split", "TransportSplitDescription"),
                ("Gross weight", "GrossWeightMeasure"),
                ("Unit", "GrossWeightMeasure/@unitCode"),
                ("Goods", "SummaryDescription"),
                ("ULD", "../UtilizedUnitLoadTransportEquipment/ID"),
                ("Unloaded at", "../../OccurrenceArrivalLocation/ID"),
            ],
        },
    ],
    custom: Some(|doc, out| {
        let numbers: Vec<String> = select(doc, 0, "**/TransportContractDocument/ID")
            .iter()
            .map(|&e| doc.text(e))
            .collect();
        awb_check(out, &numbers);
        compare(
            out,
            "Total pieces = sum of consignment pieces",
            value(doc, 0, "LogisticsTransportMovement/TotalPieceQuantity"),
            sum_at(doc, 0, "**/IncludedMasterConsignment/PieceQuantity"),
        );
    }),
};

/// Any other Cargo-XML message: the header and the business document ID.
pub static OTHER: XmlSpec = XmlSpec {
    family: "IATA Cargo-XML",
    kind: "Cargo-XML message",
    matches: |_, ns| iata(ns).is_some(),
    version,
    facts: &[
        HEADER[0],
        HEADER[1],
        HEADER[2],
        HEADER[3],
        HEADER[4],
        HEADER[5],
        F("Document", "ID", "BusinessHeaderDocument/ID"),
        F("Document", "Air waybill", "**/TransportContractDocument/ID"),
    ],
    tables: &[],
    custom: Some(|doc, out| {
        let name = doc.root().name.clone();
        out.kind = format!("{name} — Cargo-XML message (no specific view yet)");
        out.check(
            Stage::Parsed,
            State::Partial,
            "Only the message header is interpreted for this message type",
        );
    }),
};
