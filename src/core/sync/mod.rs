use anyhow::{Context, Result, anyhow};

struct ParsedSeasonKey {
    pub year: i32,
    pub season: String, // "WINTER"
    pub season_id: i32, // 202601
}

fn parse_season_key(key: &str) -> Result<ParsedSeasonKey> {
    let (year_str, season_str) = key
        .split_once('-')
        .ok_or_else(|| anyhow!("Invalid format: expected 'year-season', got '{}'", key))?;

    let year: i32 = year_str
        .parse()
        .with_context(|| format!("Failed to parse year from '{}'", year_str))?;

    let (season_upper, month_offset) = match season_str.to_lowercase().as_str() {
        "winter" => ("WINTER", 1),
        "spring" => ("SPRING", 4),
        "summer" => ("SUMMER", 7),
        "autumn" => ("AUTUMN", 10),
        _ => return Err(anyhow!("Unknown season: '{}'", season_str)),
    };

    let season_id = year * 100 + month_offset;

    Ok(ParsedSeasonKey {
        year,
        season: season_upper.to_string(),
        season_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_season_key_winter() {
        let parsed = parse_season_key("2026-winter").unwrap();
        assert_eq!(parsed.year, 2026);
        assert_eq!(parsed.season, "WINTER");
        assert_eq!(parsed.season_id, 202601);
    }

    #[test]
    fn test_parse_season_key_spring() {
        let parsed = parse_season_key("2025-spring").unwrap();
        assert_eq!(parsed.season_id, 202504);
    }

    #[test]
    fn test_parse_season_key_summer() {
        let parsed = parse_season_key("2025-summer").unwrap();
        assert_eq!(parsed.season_id, 202507);
    }

    #[test]
    fn test_parse_season_key_autumn() {
        let parsed = parse_season_key("2025-autumn").unwrap();
        assert_eq!(parsed.season_id, 202510);
    }

    #[test]
    fn test_parse_season_key_invalid() {
        assert!(parse_season_key("invalid").is_err());
        assert!(parse_season_key("2026-badseason").is_err());
    }
}
