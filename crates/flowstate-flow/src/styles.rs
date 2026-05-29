#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebateStyleKey {
  Policy,
  PublicForum,
  LincolnDouglas,
  Congress,
  WorldSchools,
  BigQuestions,
  NofSpar,
  Parli,
  Classic,
  BritishParliamentary,
  Ippf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebateStyleTemplate {
  pub key: DebateStyleKey,
  pub label: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebateStyleFlow {
  pub name: String,
  pub columns: Vec<String>,
  pub columns_switch: Option<Vec<String>>,
  pub invert: bool,
  pub starter_boxes: Option<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimerSpeech {
  pub name: String,
  pub time_ms: u32,
  pub secondary: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebateStyle {
  pub flows: Vec<DebateStyleFlow>,
  pub alternative_flows_1: Option<Vec<DebateStyleFlow>>,
  pub timer_speeches: Vec<TimerSpeech>,
  pub prep_time_ms: Option<u32>,
}

#[hotpath::measure]
pub fn all_debate_style_templates() -> Vec<DebateStyleTemplate> {
  vec![
    DebateStyleTemplate { key: DebateStyleKey::Policy, label: "Policy" },
    DebateStyleTemplate { key: DebateStyleKey::PublicForum, label: "Public Forum" },
    DebateStyleTemplate { key: DebateStyleKey::LincolnDouglas, label: "Lincoln Douglas" },
    DebateStyleTemplate { key: DebateStyleKey::Congress, label: "Congress" },
    DebateStyleTemplate { key: DebateStyleKey::WorldSchools, label: "World Schools" },
    DebateStyleTemplate { key: DebateStyleKey::BigQuestions, label: "Big Questions" },
    DebateStyleTemplate { key: DebateStyleKey::NofSpar, label: "NOF SPAR" },
    DebateStyleTemplate { key: DebateStyleKey::Parli, label: "Parli" },
    DebateStyleTemplate { key: DebateStyleKey::Classic, label: "Classic" },
    DebateStyleTemplate { key: DebateStyleKey::BritishParliamentary, label: "British Parliamentary" },
    DebateStyleTemplate { key: DebateStyleKey::Ippf, label: "International Public Policy Forum" },
  ]
}

#[hotpath::measure]
pub fn debate_style_label(key: DebateStyleKey) -> &'static str {
  all_debate_style_templates()
    .into_iter()
    .find(|template| template.key == key)
    .map(|template| template.label)
    .unwrap_or("Policy")
}

#[hotpath::measure]
pub fn debate_style_templates(key: DebateStyleKey, ld_toc_circuit: bool) -> Vec<DebateStyleFlow> {
  let style = debate_style(key);
  if key == DebateStyleKey::LincolnDouglas && ld_toc_circuit {
    style.alternative_flows_1.unwrap_or(style.flows)
  } else {
    style.flows
  }
}

#[hotpath::measure]
pub fn debate_style(key: DebateStyleKey) -> DebateStyle {
  match key {
    DebateStyleKey::Policy => DebateStyle {
      flows: vec![
        flow("aff", &["1AC", "1NC", "2AC", "2NC/1NR", "1AR", "2NR", "2AR"], false),
        flow("neg", &["1NC", "2AC", "2NC/1NR", "1AR", "2NR", "2AR"], true),
      ],
      alternative_flows_1: None,
      timer_speeches: speeches(&[
        ("1AC", 8 * 60 * 1000, false),
        ("CX", 3 * 60 * 1000, false),
        ("1NC", 8 * 60 * 1000, true),
        ("CX", 3 * 60 * 1000, true),
        ("2AC", 8 * 60 * 1000, false),
        ("CX", 3 * 60 * 1000, false),
        ("2NC", 8 * 60 * 1000, true),
        ("CX", 3 * 60 * 1000, true),
        ("1NR", 5 * 60 * 1000, true),
        ("1AR", 5 * 60 * 1000, false),
        ("2NR", 5 * 60 * 1000, true),
        ("2AR", 5 * 60 * 1000, false),
      ]),
      prep_time_ms: Some(8 * 60 * 1000),
    },
    DebateStyleKey::PublicForum => DebateStyle {
      flows: vec![
        flow_switch(
          "aff",
          &["AC", "NC", "AR", "NR", "AS", "NS", "AFF", "NFF"],
          &["AC", "NR", "AR", "NS", "AS", "NFF", "AFF"],
          false,
        ),
        flow_switch(
          "neg",
          &["NC", "AR", "NR", "AS", "NS", "AFF", "NFF"],
          &["NC", "AC", "NR", "AR", "NS", "AS", "NFF", "AFF"],
          true,
        ),
      ],
      alternative_flows_1: None,
      timer_speeches: speeches(&[
        ("AC", 4 * 60 * 1000, false),
        ("NC", 4 * 60 * 1000, true),
        ("CX", 3 * 60 * 1000, false),
        ("AR", 4 * 60 * 1000, false),
        ("NR", 4 * 60 * 1000, true),
        ("CX", 3 * 60 * 1000, false),
        ("AS", 3 * 60 * 1000, false),
        ("NS", 3 * 60 * 1000, true),
        ("GCX", 3 * 60 * 1000, false),
        ("AFF", 2 * 60 * 1000, false),
        ("NFF", 2 * 60 * 1000, true),
      ]),
      prep_time_ms: Some(3 * 60 * 1000),
    },
    DebateStyleKey::LincolnDouglas => DebateStyle {
      flows: vec![
        flow_starter("aff", &["AC", "NR", "1AR", "2NR", "2AR"], false, &["Value", "Criterion"]),
        flow_starter("neg", &["NC", "1AR", "2NR", "2AR"], true, &["Value", "Criterion"]),
      ],
      alternative_flows_1: Some(vec![
        flow_starter("aff", &["AC", "NR", "1AR", "2NR", "2AR"], false, &["type here"]),
        flow_starter("neg", &["NC", "1AR", "2NR", "2AR"], true, &["type here"]),
        flow_starter("1ar", &["1AR", "2NR", "2AR"], false, &["type here"]),
        flow_starter("2nr", &["2NR", "2AR"], true, &["type here"]),
      ]),
      timer_speeches: speeches(&[
        ("AC", 6 * 60 * 1000, false),
        ("CX", 3 * 60 * 1000, false),
        ("NC", 7 * 60 * 1000, true),
        ("CX", 3 * 60 * 1000, false),
        ("1AR", 4 * 60 * 1000, false),
        ("NR", 6 * 60 * 1000, true),
        ("2AR", 3 * 60 * 1000, false),
      ]),
      prep_time_ms: Some(4 * 60 * 1000),
    },
    DebateStyleKey::Congress => DebateStyle {
      flows: vec![flow(
        "bill",
        &[
          "1A", "Q/1N", "Q/2A", "Q/2N", "Q/3A", "Q/3N", "Q/4A", "Q/4N", "Q/5A", "Q/5N", "Q/6A", "Q/6N",
          "Q/7A", "Q/7N", "Q/8A", "Q/8N", "Q/9A", "Q/9N", "Q/10A", "Q/10N", "Q/11A", "Q/11N", "Q/12A",
          "Q/12N", "Q/13A", "Q/13N", "Q/14A", "Q/14N", "Q/15A", "Q/15N", "Q/16A", "Q/16N", "Q/17A",
          "Q/17N", "Q/18A", "Q/18N", "Q/19A", "Q/19N", "Q/20A", "Q/20N", "Q/20A", "Q/20N", "Q/21A",
          "Q/21N", "Q/22A", "Q/22N", "Q/23A", "Q/23N", "Q/24A", "Q/24N", "Q/25A", "Q/25N",
        ],
        false,
      )],
      alternative_flows_1: None,
      timer_speeches: speeches(&[("speech", 3 * 60 * 1000, false)]),
      prep_time_ms: None,
    },
    DebateStyleKey::WorldSchools => DebateStyle {
      flows: vec![
        flow("prop", &["P1", "O1", "P2", "O2", "PW", "OW", "OR", "PR"], false),
        flow("opp", &["O1", "P2", "O2", "PW", "OW", "OR", "PR"], true),
      ],
      alternative_flows_1: None,
      timer_speeches: speeches(&[
        ("P1", 8 * 60 * 1000, false),
        ("O1", 8 * 60 * 1000, true),
        ("P2", 8 * 60 * 1000, false),
        ("O2", 8 * 60 * 1000, true),
        ("PW", 8 * 60 * 1000, false),
        ("OW", 8 * 60 * 1000, true),
        ("OR", 4 * 60 * 1000, true),
        ("PR", 4 * 60 * 1000, false),
      ]),
      prep_time_ms: None,
    },
    DebateStyleKey::BigQuestions => DebateStyle {
      flows: vec![
        flow("aff", &["AC", "NC", "ARb", "NRb", "A3", "N3", "ARt", "NRt"], false),
        flow("neg", &["NC", "ARb", "NRb", "A3", "N3", "ARt", "NRt"], true),
      ],
      alternative_flows_1: None,
      timer_speeches: speeches(&[
        ("AC", 5 * 60 * 1000, false),
        ("NC", 5 * 60 * 1000, true),
        ("QS", 3 * 60 * 1000, false),
        ("ARb", 4 * 60 * 1000, false),
        ("NRb", 4 * 60 * 1000, true),
        ("QS", 3 * 60 * 1000, false),
        ("A3", 3 * 60 * 1000, false),
        ("N3", 3 * 60 * 1000, true),
        ("ARt", 3 * 60 * 1000, false),
        ("NRt", 3 * 60 * 1000, true),
      ]),
      prep_time_ms: Some(3 * 60 * 1000),
    },
    DebateStyleKey::NofSpar => DebateStyle {
      flows: vec![
        flow("pro", &["PC", "CC", "PR", "CR"], false),
        flow("con", &["CC", "PR", "CR"], true),
      ],
      alternative_flows_1: None,
      timer_speeches: speeches(&[
        ("PREP", 2 * 60 * 1000, false),
        ("PC", 2 * 60 * 1000, false),
        ("CC", 2 * 60 * 1000, true),
        ("CX", 4 * 60 * 1000, false),
        ("PR", 2 * 60 * 1000, false),
        ("CR", 2 * 60 * 1000, true),
      ]),
      prep_time_ms: None,
    },
    DebateStyleKey::Parli => DebateStyle {
      flows: vec![
        flow("pro", &["1PC", "1OC", "2PC", "2OC/OR", "PR"], false),
        flow("opp", &["1OC", "2PC", "2OC/OR", "PR"], true),
      ],
      alternative_flows_1: None,
      timer_speeches: speeches(&[
        ("1PC", 7 * 60 * 1000, false),
        ("1OC", 8 * 60 * 1000, true),
        ("2PC", 8 * 60 * 1000, false),
        ("2OC", 8 * 60 * 1000, true),
        ("OR", 4 * 60 * 1000, true),
        ("PR", 5 * 60 * 1000, false),
      ]),
      prep_time_ms: None,
    },
    DebateStyleKey::Classic => DebateStyle {
      flows: vec![
        flow("aff", &["AC", "NC/1NR", "1AR", "2NR", "2AR", "NS", "AS"], false),
        flow("neg", &["NC/1NR", "1AR", "2NR", "2AR", "AS", "NS"], true),
      ],
      alternative_flows_1: None,
      timer_speeches: speeches(&[
        ("AC", 6 * 60 * 1000, false),
        ("CX", 3 * 60 * 1000, true),
        ("NC", 6 * 60 * 1000, true),
        ("CX", 3 * 60 * 1000, false),
        ("1NR", 5 * 60 * 1000, true),
        ("CX", 3 * 60 * 1000, false),
        ("prep", 2 * 60 * 1000, false),
        ("1AR", 7 * 60 * 1000, false),
        ("CX", 3 * 60 * 1000, true),
        ("prep", 2 * 60 * 1000, true),
        ("2NR", 6 * 60 * 1000, true),
        ("prep", 2 * 60 * 1000, false),
        ("2AR", 4 * 60 * 1000, false),
        ("prep", 2 * 60 * 1000, true),
        ("NS", 3 * 60 * 1000, true),
        ("prep", 2 * 60 * 1000, false),
        ("AS", 3 * 60 * 1000, false),
      ]),
      prep_time_ms: None,
    },
    DebateStyleKey::BritishParliamentary => DebateStyle {
      flows: vec![
        flow("OG", &["PM", "LO", "DPM", "DLO", "MG", "MO", "GW", "CW"], false),
        flow("OO", &["LO", "DPM", "DLO", "MG", "MO", "GW", "CW"], true),
        flow("CG", &["MG", "MO", "GW", "CW"], false),
        flow("CO", &["MO", "GW", "CW"], true),
      ],
      alternative_flows_1: None,
      timer_speeches: speeches(&[
        ("PM", 7 * 60 * 1000, false),
        ("LO", 7 * 60 * 1000, true),
        ("DPM", 7 * 60 * 1000, false),
        ("DLO", 7 * 60 * 1000, true),
        ("MG", 7 * 60 * 1000, false),
        ("MO", 7 * 60 * 1000, true),
        ("GW", 7 * 60 * 1000, false),
        ("CW", 7 * 60 * 1000, true),
      ]),
      prep_time_ms: Some(15 * 1000),
    },
    DebateStyleKey::Ippf => DebateStyle {
      flows: vec![
        flow("aff", &["A1", "N1", "A2", "N2", "AR", "NR"], false),
        flow("neg", &["N1", "A2", "N2", "AR", "NR"], true),
      ],
      alternative_flows_1: None,
      timer_speeches: speeches(&[
        ("A1", 5 * 60 * 1000, false),
        ("N1", 5 * 60 * 1000, true),
        ("BREAK", 90 * 1000, false),
        ("A2", 5 * 60 * 1000, false),
        ("N2", 5 * 60 * 1000, true),
        ("ACX", 60 * 1000, false),
        ("NCX", 60 * 1000, true),
        ("AR", 5 * 60 * 1000, false),
        ("NR", 5 * 60 * 1000, true),
      ]),
      prep_time_ms: None,
    },
  }
}

#[hotpath::measure]
fn flow(name: &str, columns: &[&str], invert: bool) -> DebateStyleFlow {
  DebateStyleFlow {
    name: name.to_string(),
    columns: strings(columns),
    columns_switch: None,
    invert,
    starter_boxes: None,
  }
}

#[hotpath::measure]
fn flow_switch(name: &str, columns: &[&str], columns_switch: &[&str], invert: bool) -> DebateStyleFlow {
  DebateStyleFlow {
    name: name.to_string(),
    columns: strings(columns),
    columns_switch: Some(strings(columns_switch)),
    invert,
    starter_boxes: None,
  }
}

#[hotpath::measure]
fn flow_starter(name: &str, columns: &[&str], invert: bool, starter_boxes: &[&str]) -> DebateStyleFlow {
  DebateStyleFlow {
    name: name.to_string(),
    columns: strings(columns),
    columns_switch: None,
    invert,
    starter_boxes: Some(strings(starter_boxes)),
  }
}

#[hotpath::measure]
fn strings(values: &[&str]) -> Vec<String> {
  values.iter().map(|value| (*value).to_string()).collect()
}

#[hotpath::measure]
fn speeches(values: &[(&str, u32, bool)]) -> Vec<TimerSpeech> {
  values
    .iter()
    .map(|(name, time_ms, secondary)| TimerSpeech {
      name: (*name).to_string(),
      time_ms: *time_ms,
      secondary: *secondary,
    })
    .collect()
}
