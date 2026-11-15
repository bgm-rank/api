use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct MalSeasonalResponse {
    #[serde(default)]
    pub data: Vec<MalSeasonalEntry>,
    #[serde(default)]
    pub paging: MalPaging,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct MalPaging {
    pub next: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct MalSeasonalEntry {
    pub node: MalAnimeNode,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct MalAnimeNode {
    pub id: i32,
    pub title: String,
    #[serde(default)]
    pub alternative_titles: Option<MalAlternativeTitles>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub synopsis: Option<String>,
    pub media_type: Option<String>,
    pub status: Option<String>,
    pub num_episodes: Option<i32>,
    pub start_season: Option<MalStartSeason>,
    pub broadcast: Option<MalBroadcast>,
    pub source: Option<String>,
    #[serde(default)]
    pub studios: Vec<MalStudio>,
    pub rating: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct MalAlternativeTitles {
    #[serde(default)]
    pub synonyms: Vec<String>,
    pub en: Option<String>,
    pub ja: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct MalStartSeason {
    pub year: i32,
    pub season: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct MalBroadcast {
    pub day_of_the_week: Option<String>,
    pub start_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct MalStudio {
    pub id: Option<i32>,
    pub name: String,
}
