//! Bundled world basemap: coastline rings from a simplified (Natural Earth
//! 110m) land GeoJSON, for the hosts map background.

use std::sync::OnceLock;

/// Coastline rings as `[lng, lat]` point lists; parsed lazily and cached.
pub fn coastlines() -> &'static [Vec<[f32; 2]>] {
    static RINGS: OnceLock<Vec<Vec<[f32; 2]>>> = OnceLock::new();
    RINGS.get_or_init(parse)
}

fn parse() -> Vec<Vec<[f32; 2]>> {
    const RAW: &str = include_str!("../assets/ne_110m_land.geojson");
    let mut rings = Vec::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(RAW) else {
        log::warn!("failed to parse bundled world map");
        return rings;
    };
    let Some(features) = value.get("features").and_then(|f| f.as_array()) else {
        return rings;
    };
    for feature in features {
        let Some(geom) = feature.get("geometry") else {
            continue;
        };
        let Some(coords) = geom.get("coordinates") else {
            continue;
        };
        match geom.get("type").and_then(|t| t.as_str()) {
            Some("Polygon") => push_polygon(coords, &mut rings),
            Some("MultiPolygon") => {
                for poly in coords.as_array().into_iter().flatten() {
                    push_polygon(poly, &mut rings);
                }
            }
            _ => {}
        }
    }
    rings
}

/// Each ring of the polygon becomes its own closed polyline.
fn push_polygon(poly: &serde_json::Value, out: &mut Vec<Vec<[f32; 2]>>) {
    for ring in poly.as_array().into_iter().flatten() {
        let line: Vec<[f32; 2]> = ring
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                let pair = p.as_array()?;
                Some([
                    pair.first()?.as_f64()? as f32,
                    pair.get(1)?.as_f64()? as f32,
                ])
            })
            .collect();
        if line.len() >= 2 {
            out.push(line);
        }
    }
}
