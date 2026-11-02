//! DDI (Data Documentation Initiative) metadata for social-science studies: Codebook 2.x
//! (`ddi:codebook:2_5`) and Lifecycle 3.x instances (`ddi:instance:3_*`). Study citation,
//! variables with their labels, questions and categories.
use super::support::value;
use crate::inspect::{
    spec::{F, T, XmlSpec},
    xmldoc::Doc,
};

fn version_from_ns(doc: &Doc) -> Option<String> {
    let ns = &doc.root().ns;
    let v = ns.rsplit(':').next()?;
    v.contains('_').then(|| v.replace('_', "."))
}

pub static CODEBOOK: XmlSpec = XmlSpec {
    family: "DDI",
    kind: "DDI Codebook",
    matches: |root, ns| {
        root == "codeBook"
            && (ns.starts_with("ddi:codebook:")
                || ns.contains("ddialliance.org")
                || ns.contains("icpsr.umich.edu/DDI")
                || ns.is_empty())
    },
    version: |doc| {
        version_from_ns(doc)
            .or_else(|| value(doc, 0, "@version"))
            .map(|v| format!("Codebook {v}"))
    },
    facts: &[
        F("Study", "Title", "stdyDscr/citation/titlStmt/titl"),
        F("Study", "Subtitle", "stdyDscr/citation/titlStmt/subTitl"),
        F(
            "Study",
            "Alternative title",
            "stdyDscr/citation/titlStmt/altTitl",
        ),
        F("Study", "Identifier", "stdyDscr/citation/titlStmt/IDNo"),
        F(
            "Study",
            "Authors",
            "count:stdyDscr/citation/rspStmt/AuthEnty",
        ),
        F(
            "Study",
            "First author",
            "stdyDscr/citation/rspStmt/AuthEnty",
        ),
        F("Study", "Producer", "stdyDscr/citation/prodStmt/producer"),
        F(
            "Study",
            "Production date",
            "stdyDscr/citation/prodStmt/prodDate/@date | stdyDscr/citation/prodStmt/prodDate",
        ),
        F(
            "Study",
            "Distributor",
            "stdyDscr/citation/distStmt/distrbtr",
        ),
        F(
            "Study",
            "Distribution date",
            "stdyDscr/citation/distStmt/distDate/@date | stdyDscr/citation/distStmt/distDate",
        ),
        F("Study", "Version", "stdyDscr/citation/verStmt/version"),
        F(
            "Coverage",
            "Collection start",
            "stdyDscr/stdyInfo/sumDscr/collDate[@event=start]/@date | stdyDscr/stdyInfo/sumDscr/collDate/@date",
        ),
        F(
            "Coverage",
            "Collection end",
            "stdyDscr/stdyInfo/sumDscr/collDate[@event=end]/@date",
        ),
        F(
            "Coverage",
            "Time period",
            "stdyDscr/stdyInfo/sumDscr/timePrd/@date",
        ),
        F("Coverage", "Country", "stdyDscr/stdyInfo/sumDscr/nation"),
        F("Coverage", "Universe", "stdyDscr/stdyInfo/sumDscr/universe"),
        F(
            "Coverage",
            "Unit of analysis",
            "stdyDscr/stdyInfo/sumDscr/anlyUnit",
        ),
        F("Method", "Sampling", "stdyDscr/method/dataColl/sampProc"),
        F(
            "Method",
            "Collection mode",
            "stdyDscr/method/dataColl/collMode",
        ),
        F("Files", "File", "fileDscr/fileTxt/fileName"),
        F("Files", "Cases", "fileDscr/fileTxt/dimensns/caseQnty"),
        F(
            "Files",
            "Variables (declared)",
            "fileDscr/fileTxt/dimensns/varQnty",
        ),
        F("Files", "Variables", "count:dataDscr/var"),
        F("Files", "Variable groups", "count:dataDscr/varGrp"),
    ],
    tables: &[
        T {
            name: "Variables",
            rows: "dataDscr/var",
            columns: &[
                ("Name", "@name"),
                ("Label", "labl"),
                ("Type", "varFormat/@type"),
                ("Interval", "@intrvl"),
                ("Question", "qstn/qstnLit"),
                ("Categories", "count:catgry"),
                ("File", "@files"),
                ("ID", "@ID"),
            ],
        },
        T {
            name: "Categories",
            rows: "dataDscr/var/catgry",
            columns: &[
                ("Variable", "../@name"),
                ("Value", "catValu"),
                ("Label", "labl"),
                ("Frequency", "catStat[@type=freq] | catStat"),
                ("Missing", "@missing"),
            ],
        },
        T {
            name: "Authors",
            rows: "stdyDscr/citation/rspStmt/AuthEnty",
            columns: &[("Name", "."), ("Affiliation", "@affiliation")],
        },
        T {
            name: "Variable groups",
            rows: "dataDscr/varGrp",
            columns: &[
                ("Name", "@name | labl"),
                ("Label", "labl"),
                ("Variables", "@var"),
            ],
        },
    ],
    custom: Some(|_, out| {
        out.notes
            .push("Summary statistics are shown as recorded; the data file is not read".into());
    }),
};

pub static LIFECYCLE: XmlSpec = XmlSpec {
    family: "DDI",
    kind: "DDI Lifecycle instance",
    matches: |root, ns| {
        (root == "DDIInstance" || root == "FragmentInstance") && ns.starts_with("ddi:instance:3")
    },
    version: |doc| version_from_ns(doc).map(|v| format!("Lifecycle {v}")),
    facts: &[
        F(
            "Study",
            "Title",
            "**/StudyUnit/Citation/Title/String | **/Citation/Title/String",
        ),
        F("Study", "Subtitle", "**/StudyUnit/Citation/SubTitle/String"),
        F(
            "Study",
            "Creator",
            "**/StudyUnit/Citation/Creator/CreatorName/String | **/Citation/Creator/CreatorName/String",
        ),
        F("Study", "Creators", "count:**/StudyUnit/Citation/Creator"),
        F(
            "Study",
            "Publisher",
            "**/StudyUnit/Citation/Publisher/PublisherName/String",
        ),
        F(
            "Study",
            "Publication date",
            "**/StudyUnit/Citation/PublicationDate/SimpleDate",
        ),
        F("Study", "Agency", "**/StudyUnit/Agency | @agency"),
        F("Study", "ID", "**/StudyUnit/ID"),
        F("Study", "Abstract", "**/StudyUnit/Abstract/Content"),
        F("Content", "Variables", "count:**/Variable"),
        F("Content", "Questions", "count:**/QuestionItem"),
        F("Content", "Categories", "count:**/Category"),
        F("Content", "Code lists", "count:**/CodeList"),
    ],
    tables: &[
        T {
            name: "Variables",
            rows: "**/Variable",
            columns: &[
                ("Name", "VariableName/String"),
                ("Label", "Label/Content"),
                (
                    "Numeric type",
                    "VariableRepresentation/NumericRepresentation/NumericTypeCode",
                ),
                (
                    "Code list",
                    "VariableRepresentation/CodeRepresentation/CodeListReference/ID",
                ),
                ("Question", "QuestionReference/ID"),
                ("ID", "ID"),
            ],
        },
        T {
            name: "Questions",
            rows: "**/QuestionItem",
            columns: &[
                ("Name", "QuestionItemName/String"),
                ("Text", "QuestionText/LiteralText/Text"),
                ("ID", "ID"),
            ],
        },
        T {
            name: "Categories",
            rows: "**/Category",
            columns: &[
                ("Name", "CategoryName/String"),
                ("Label", "Label/Content"),
                ("ID", "ID"),
            ],
        },
        T {
            name: "Codes",
            rows: "**/CodeList/Code",
            columns: &[
                ("Code list", "../ID"),
                ("Value", "Value"),
                ("Category", "CategoryReference/ID"),
            ],
        },
    ],
    custom: Some(|_, out| {
        out.notes.push(
            "References between items (variable → question, code → category) are shown as IDs; they are not resolved"
                .into(),
        );
    }),
};
