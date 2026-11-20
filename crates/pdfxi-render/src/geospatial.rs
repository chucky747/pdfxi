//! Geospatial PDF model (feature MEDIA-004).
//!
//! This module models the georeference structures used by geospatial PDFs
//! without performing any mapping-library or projection work beyond simple,
//! deterministic affine interpolation. It understands:
//!
//! * `/VP` viewport arrays (a PDF rectangle paired with its geospatial
//!   counterparts in `/GPTS`);
//! * `/Measure` scale and unit metadata (`/Subtype`, `/XStep`, `/YStep`, unit);
//! * the `/GPTS`, `/LPTS` and `/GCS` georeference dictionaries; and
//! * bounds validation for every numeric input.
//!
//! # Backend limits (honest delegation)
//!
//! Coordinate mapping here is a linear interpolation inside one viewport, valid
//! for small extents in a geographic (longitude/latitude) reference system.
//! It is intentionally *not* a projection engine: a `/GCS` whose projection is
//! not [`Projection::Geographic`] is reported through
//! [`GeospatialError::UnsupportedProjection`] and a matching
//! [`PlanNote::UnsupportedProjection`] rather than being silently mis-mapped.
//! A native geospatial backend must supply real datum and projection handling
//! for those cases.
//!
//! All helpers are pure and deterministic: identical inputs yield identical
//! floating-point results, every bounded input is capped, and no filesystem,
//! wall-clock, randomness, unsafe code or new dependency is used. A parse or
//! mapping is short, so no cancellation token is required.

/// Maximum number of viewports in one geospatial document.
pub const MAX_VIEWPORTS: usize = 32;
/// Maximum absolute magnitude accepted for any coordinate value.
pub const MAX_COORD_ABS: f64 = 1.0e9;
/// Maximum byte length of a georeference name.
pub const MAX_NAME_BYTES: usize = 128;
/// Inclusive tolerance used at viewport edges during bounds checks.
pub const EDGE_TOLERANCE: f64 = 1.0e-9;

/// Structured errors produced while parsing or mapping geospatial data.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum GeospatialError {
    /// No viewports were declared.
    #[error("geospatial document must declare at least one viewport")]
    EmptyViewports,
    /// More viewports were declared than the cap allows.
    #[error("viewport count {count} exceeds the {limit}-viewport limit")]
    TooManyViewports {
        /// Number of viewports supplied.
        count: usize,
        /// Enforced viewport limit.
        limit: usize,
    },
    /// A viewport index was out of range.
    #[error("viewport index {index} is out of range (count {count})")]
    ViewportIndexOutOfRange {
        /// The rejected index.
        index: usize,
        /// Number of available viewports.
        count: usize,
    },
    /// A coordinate was not finite.
    #[error("coordinate '{field}' must be finite, got {value}")]
    NonFiniteCoordinate {
        /// Name of the offending field.
        field: &'static str,
        /// The rejected value.
        value: f64,
    },
    /// A coordinate exceeded the bounded magnitude.
    #[error("coordinate '{field}' magnitude {value} exceeds {limit}")]
    CoordinateOutOfRange {
        /// Name of the offending field.
        field: &'static str,
        /// The rejected value.
        value: f64,
        /// Enforced magnitude limit.
        limit: f64,
    },
    /// A PDF viewport rectangle had non-positive extent.
    #[error("viewport PDF rectangle must satisfy x1 > x0 and y1 > y0")]
    InvalidViewportRectangle,
    /// A `/GPTS` bound was invalid (reversed or outside longitude/latitude).
    #[error(
        "GPTS bounds must satisfy lon1 > lon0, lat1 > lat0, lon in -180..=180 and lat in -90..=90"
    )]
    InvalidGeoBounds,
    /// A `/LPTS` local rectangle had non-positive extent.
    #[error("LPTS rectangle must satisfy x1 > x0 and y1 > y0")]
    InvalidLocalPoints,
    /// A measure step was not finite and positive.
    #[error("measure step '{field}' must be finite and positive, got {value}")]
    InvalidMeasureStep {
        /// Name of the offending field.
        field: &'static str,
        /// The rejected value.
        value: f64,
    },
    /// A unit name was empty or too long.
    #[error("measure unit name must be 1..={limit} bytes")]
    InvalidUnitName {
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// A coordinate-system name was empty or too long.
    #[error("GCS name must be 1..={limit} bytes")]
    InvalidGcsName {
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// `/LPTS` data was requested but absent.
    #[error("geospatial document has no LPTS dictionary")]
    NoLocalPoints,
    /// A point fell outside the selected viewport.
    #[error("point ({x}, {y}) is outside the selected viewport")]
    PointOutsideViewport {
        /// The rejected x or longitude value.
        x: f64,
        /// The rejected y or latitude value.
        y: f64,
    },
    /// The coordinate system uses a projection this model does not support.
    #[error("unsupported geospatial projection '{0}'")]
    UnsupportedProjection(String),
}

/// A unit of measure parsed from a `/Measure` dictionary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Metres (SI base for linear units).
    Metre,
    /// Kilometres.
    Kilometre,
    /// International feet.
    Foot,
    /// US survey feet.
    UsSurveyFoot,
    /// Inches.
    Inch,
    /// Statute miles.
    Mile,
    /// Nautical miles.
    NauticalMile,
    /// Degrees (base unit for angular measures).
    Degree,
    /// Radians (SI base for angular units).
    Radian,
    /// An unrecognised unit, preserved for honest reporting.
    Unknown(String),
}

impl Unit {
    /// Parse a unit name case-insensitively.
    pub fn parse(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "m" | "meter" | "metre" | "meters" | "metres" => Self::Metre,
            "km" | "kilometer" | "kilometre" => Self::Kilometre,
            "ft" | "foot" | "feet" => Self::Foot,
            "usft" | "us survey foot" | "survey foot" => Self::UsSurveyFoot,
            "in" | "inch" | "inches" => Self::Inch,
            "mi" | "mile" | "miles" => Self::Mile,
            "nmi" | "nautical mile" => Self::NauticalMile,
            "deg" | "degree" | "degrees" => Self::Degree,
            "rad" | "radian" | "radians" => Self::Radian,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Return `true` when this is a linear unit.
    pub fn is_linear(&self) -> bool {
        matches!(
            self,
            Self::Metre
                | Self::Kilometre
                | Self::Foot
                | Self::UsSurveyFoot
                | Self::Inch
                | Self::Mile
                | Self::NauticalMile
        )
    }

    /// Return `true` when this is an angular unit.
    pub fn is_angular(&self) -> bool {
        matches!(self, Self::Degree | Self::Radian)
    }

    /// Return the multiplicative factor to the base unit (metres or radians),
    /// or `None` for an unknown unit.
    pub fn to_base_factor(&self) -> Option<f64> {
        Some(match self {
            Self::Metre => 1.0,
            Self::Kilometre => 1000.0,
            Self::Foot => 0.304_8,
            Self::UsSurveyFoot => 1200.0 / 3937.0,
            Self::Inch => 0.025_4,
            Self::Mile => 1609.344,
            Self::NauticalMile => 1852.0,
            Self::Degree => std::f64::consts::PI / 180.0,
            Self::Radian => 1.0,
            Self::Unknown(_) => return None,
        })
    }
}

/// The kind of measure described by a `/Measure` dictionary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasureSubtype {
    /// A linear distance measure.
    Linear,
    /// An angular measure.
    Angular,
}

impl MeasureSubtype {
    /// Parse a `/Subtype` name case-insensitively.
    pub fn parse(name: &str) -> Self {
        if name.eq_ignore_ascii_case("Angular") {
            Self::Angular
        } else {
            Self::Linear
        }
    }
}

/// A validated `/Measure` dictionary.
#[derive(Clone, Debug, PartialEq)]
pub struct Measure {
    subtype: MeasureSubtype,
    x_step: f64,
    y_step: f64,
    unit: Unit,
}

impl Measure {
    /// Validate and build a measure from its dictionary fields.
    pub fn parse(
        subtype_name: &str,
        x_step: f64,
        y_step: f64,
        unit_name: &str,
    ) -> Result<Self, GeospatialError> {
        if unit_name.is_empty() || unit_name.len() > MAX_NAME_BYTES {
            return Err(GeospatialError::InvalidUnitName {
                limit: MAX_NAME_BYTES,
            });
        }
        if !x_step.is_finite() || x_step <= 0.0 {
            return Err(GeospatialError::InvalidMeasureStep {
                field: "XStep",
                value: x_step,
            });
        }
        if !y_step.is_finite() || y_step <= 0.0 {
            return Err(GeospatialError::InvalidMeasureStep {
                field: "YStep",
                value: y_step,
            });
        }
        Ok(Self {
            subtype: MeasureSubtype::parse(subtype_name),
            x_step,
            y_step,
            unit: Unit::parse(unit_name),
        })
    }

    /// Return the measure subtype.
    pub fn subtype(&self) -> MeasureSubtype {
        self.subtype
    }

    /// Return the horizontal step in the declared unit.
    pub fn x_step(&self) -> f64 {
        self.x_step
    }

    /// Return the vertical step in the declared unit.
    pub fn y_step(&self) -> f64 {
        self.y_step
    }

    /// Return the declared unit.
    pub fn unit(&self) -> &Unit {
        &self.unit
    }

    /// Return the horizontal step converted to the SI base unit, or `None`
    /// when the unit is unknown or does not match the measure subtype.
    pub fn x_step_base(&self) -> Option<f64> {
        self.to_base(self.x_step)
    }

    /// Return the vertical step converted to the SI base unit, or `None` when
    /// the unit is unknown or does not match the measure subtype.
    pub fn y_step_base(&self) -> Option<f64> {
        self.to_base(self.y_step)
    }

    fn to_base(&self, step: f64) -> Option<f64> {
        let factor = self.unit.to_base_factor()?;
        let compatible = match self.subtype {
            MeasureSubtype::Linear => self.unit.is_linear(),
            MeasureSubtype::Angular => self.unit.is_angular(),
        };
        if compatible {
            Some(step * factor)
        } else {
            None
        }
    }
}

/// A validated PDF-space rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfRect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl PdfRect {
    /// Validate and build a rectangle from `[x0, y0, x1, y1]`.
    pub fn new(values: [f64; 4]) -> Result<Self, GeospatialError> {
        let [x0, y0, x1, y1] = values;
        validate_coordinate("x0", x0)?;
        validate_coordinate("y0", y0)?;
        validate_coordinate("x1", x1)?;
        validate_coordinate("y1", y1)?;
        if x1 <= x0 || y1 <= y0 {
            return Err(GeospatialError::InvalidViewportRectangle);
        }
        Ok(Self { x0, y0, x1, y1 })
    }

    /// Return the lower x edge.
    pub fn x0(&self) -> f64 {
        self.x0
    }
    /// Return the lower y edge.
    pub fn y0(&self) -> f64 {
        self.y0
    }
    /// Return the upper x edge.
    pub fn x1(&self) -> f64 {
        self.x1
    }
    /// Return the upper y edge.
    pub fn y1(&self) -> f64 {
        self.y1
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 - EDGE_TOLERANCE
            && x <= self.x1 + EDGE_TOLERANCE
            && y >= self.y0 - EDGE_TOLERANCE
            && y <= self.y1 + EDGE_TOLERANCE
    }
}

/// A validated `/GPTS` longitude/latitude bound pair.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoBounds {
    lon0: f64,
    lat0: f64,
    lon1: f64,
    lat1: f64,
}

impl GeoBounds {
    /// Validate and build a `/GPTS` pair from `[lon0, lat0, lon1, lat1]`.
    pub fn new(values: [f64; 4]) -> Result<Self, GeospatialError> {
        let [lon0, lat0, lon1, lat1] = values;
        validate_coordinate("lon0", lon0)?;
        validate_coordinate("lat0", lat0)?;
        validate_coordinate("lon1", lon1)?;
        validate_coordinate("lat1", lat1)?;
        let lon_ok =
            (-180.0..=180.0).contains(&lon0) && (-180.0..=180.0).contains(&lon1) && lon1 > lon0;
        let lat_ok =
            (-90.0..=90.0).contains(&lat0) && (-90.0..=90.0).contains(&lat1) && lat1 > lat0;
        if !lon_ok || !lat_ok {
            return Err(GeospatialError::InvalidGeoBounds);
        }
        Ok(Self {
            lon0,
            lat0,
            lon1,
            lat1,
        })
    }

    /// Return the western longitude.
    pub fn lon0(&self) -> f64 {
        self.lon0
    }
    /// Return the southern latitude.
    pub fn lat0(&self) -> f64 {
        self.lat0
    }
    /// Return the eastern longitude.
    pub fn lon1(&self) -> f64 {
        self.lon1
    }
    /// Return the northern latitude.
    pub fn lat1(&self) -> f64 {
        self.lat1
    }

    fn contains(&self, lon: f64, lat: f64) -> bool {
        lon >= self.lon0 - EDGE_TOLERANCE
            && lon <= self.lon1 + EDGE_TOLERANCE
            && lat >= self.lat0 - EDGE_TOLERANCE
            && lat <= self.lat1 + EDGE_TOLERANCE
    }
}

/// A validated `/LPTS` local-point rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalPoints {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl LocalPoints {
    /// Validate and build a local-point rectangle.
    pub fn new(values: [f64; 4]) -> Result<Self, GeospatialError> {
        let [x0, y0, x1, y1] = values;
        validate_coordinate("x0", x0)?;
        validate_coordinate("y0", y0)?;
        validate_coordinate("x1", x1)?;
        validate_coordinate("y1", y1)?;
        if x1 <= x0 || y1 <= y0 {
            return Err(GeospatialError::InvalidLocalPoints);
        }
        Ok(Self { x0, y0, x1, y1 })
    }

    /// Return the lower local x edge.
    pub fn x0(&self) -> f64 {
        self.x0
    }
    /// Return the lower local y edge.
    pub fn y0(&self) -> f64 {
        self.y0
    }
    /// Return the upper local x edge.
    pub fn x1(&self) -> f64 {
        self.x1
    }
    /// Return the upper local y edge.
    pub fn y1(&self) -> f64 {
        self.y1
    }
}

/// The projection declared by a `/GCS` dictionary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Projection {
    /// Geographic longitude/latitude, which this model can map affinely.
    Geographic,
    /// Web Mercator. Honestly reported as unsupported here.
    WebMercator,
    /// Any other projection, preserved verbatim.
    Unsupported(String),
}

impl Projection {
    /// Parse a projection or coordinate-system name.
    pub fn parse(name: &str) -> Self {
        let lower = name.to_ascii_lowercase();
        if lower.contains("mercator") || lower.contains("3857") {
            Self::WebMercator
        } else if lower.contains("geographic")
            || lower.contains("wgs84")
            || lower.contains("4326")
            || lower.contains("nad83")
        {
            Self::Geographic
        } else {
            Self::Unsupported(name.to_owned())
        }
    }

    /// Return `true` only for the projection this model can map.
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Geographic)
    }
}

/// A validated `/GCS` geodetic coordinate system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeodeticCr {
    name: String,
    epsg: Option<u32>,
    projection: Projection,
}

impl GeodeticCr {
    /// Build a geodetic coordinate system from a name and optional EPSG code.
    pub fn new(name: &str, epsg: Option<u32>) -> Result<Self, GeospatialError> {
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Err(GeospatialError::InvalidGcsName {
                limit: MAX_NAME_BYTES,
            });
        }
        Ok(Self {
            name: name.to_owned(),
            epsg,
            projection: Projection::parse(name),
        })
    }

    /// Return the coordinate-system name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the EPSG code, if one was supplied.
    pub fn epsg(&self) -> Option<u32> {
        self.epsg
    }

    /// Return the parsed projection.
    pub fn projection(&self) -> &Projection {
        &self.projection
    }
}

/// One geospatial viewport: a PDF rectangle, its `/GPTS` bounds and measure.
#[derive(Clone, Debug, PartialEq)]
pub struct Viewport {
    pdf: PdfRect,
    gpts: GeoBounds,
    measure: Measure,
}

impl Viewport {
    /// Return the PDF-space rectangle.
    pub fn pdf(&self) -> &PdfRect {
        &self.pdf
    }

    /// Return the geospatial bounds.
    pub fn gpts(&self) -> &GeoBounds {
        &self.gpts
    }

    /// Return the viewport's measure dictionary.
    pub fn measure(&self) -> &Measure {
        &self.measure
    }
}

/// Owned intermediate form of one viewport dictionary.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportInput {
    /// The `/VP` rectangle as `[x0, y0, x1, y1]`.
    pub pdf_rect: [f64; 4],
    /// The `/GPTS` array as `[lon0, lat0, lon1, lat1]`.
    pub gpts: [f64; 4],
    /// The viewport's measure subtype name.
    pub measure_subtype: String,
    /// The measure `/XStep`.
    pub x_step: f64,
    /// The measure `/YStep`.
    pub y_step: f64,
    /// The measure unit name.
    pub measure_unit: String,
}

/// Owned intermediate form of a geospatial document dictionary.
#[derive(Clone, Debug, PartialEq)]
pub struct GeospatialDictionary {
    /// The declared viewports.
    pub viewports: Vec<ViewportInput>,
    /// The `/GCS` name.
    pub gcs_name: String,
    /// The `/GCS` EPSG code, when known.
    pub gcs_epsg: Option<u32>,
    /// The `/LPTS` array as `[x0, y0, x1, y1]`, when present.
    pub lpts: Option<[f64; 4]>,
}

/// A validated geospatial document model.
#[derive(Clone, Debug, PartialEq)]
pub struct GeospatialDocument {
    viewports: Vec<Viewport>,
    gcs: GeodeticCr,
    lpts: Option<LocalPoints>,
}

impl GeospatialDocument {
    /// Validate and build a geospatial document from an intermediate dictionary.
    pub fn parse(dict: GeospatialDictionary) -> Result<Self, GeospatialError> {
        if dict.viewports.is_empty() {
            return Err(GeospatialError::EmptyViewports);
        }
        if dict.viewports.len() > MAX_VIEWPORTS {
            return Err(GeospatialError::TooManyViewports {
                count: dict.viewports.len(),
                limit: MAX_VIEWPORTS,
            });
        }
        let mut viewports = Vec::with_capacity(dict.viewports.len());
        for input in dict.viewports {
            viewports.push(Viewport {
                pdf: PdfRect::new(input.pdf_rect)?,
                gpts: GeoBounds::new(input.gpts)?,
                measure: Measure::parse(
                    &input.measure_subtype,
                    input.x_step,
                    input.y_step,
                    &input.measure_unit,
                )?,
            });
        }
        let gcs = GeodeticCr::new(&dict.gcs_name, dict.gcs_epsg)?;
        let lpts = match dict.lpts {
            Some(values) => Some(LocalPoints::new(values)?),
            None => None,
        };
        Ok(Self {
            viewports,
            gcs,
            lpts,
        })
    }

    /// Return the declared viewports.
    pub fn viewports(&self) -> &[Viewport] {
        &self.viewports
    }

    /// Return the geodetic coordinate system.
    pub fn gcs(&self) -> &GeodeticCr {
        &self.gcs
    }

    /// Return the `/LPTS` dictionary, when present.
    pub fn lpts(&self) -> Option<&LocalPoints> {
        self.lpts.as_ref()
    }

    fn check_projection(&self) -> Result<(), GeospatialError> {
        if self.gcs.projection().is_supported() {
            Ok(())
        } else {
            Err(GeospatialError::UnsupportedProjection(
                self.gcs.projection().unsupported_label().to_owned(),
            ))
        }
    }

    /// Map a PDF-space point to longitude/latitude inside one viewport.
    ///
    /// This is a deterministic affine interpolation and is only defined for a
    /// geographic coordinate system. Points outside the viewport are rejected.
    pub fn pdf_to_geo(
        &self,
        viewport_index: usize,
        x: f64,
        y: f64,
    ) -> Result<(f64, f64), GeospatialError> {
        self.check_projection()?;
        let viewport = self.viewport(viewport_index)?;
        if !viewport.pdf.contains(x, y) {
            return Err(GeospatialError::PointOutsideViewport { x, y });
        }
        let tx = (x - viewport.pdf.x0()) / (viewport.pdf.x1() - viewport.pdf.x0());
        let ty = (y - viewport.pdf.y0()) / (viewport.pdf.y1() - viewport.pdf.y0());
        let lon = viewport.gpts.lon0() + tx * (viewport.gpts.lon1() - viewport.gpts.lon0());
        let lat = viewport.gpts.lat0() + ty * (viewport.gpts.lat1() - viewport.gpts.lat0());
        Ok((lon, lat))
    }

    /// Map a longitude/latitude point back to PDF space inside one viewport.
    pub fn geo_to_pdf(
        &self,
        viewport_index: usize,
        lon: f64,
        lat: f64,
    ) -> Result<(f64, f64), GeospatialError> {
        self.check_projection()?;
        let viewport = self.viewport(viewport_index)?;
        if !viewport.gpts.contains(lon, lat) {
            return Err(GeospatialError::PointOutsideViewport { x: lon, y: lat });
        }
        let tx = (lon - viewport.gpts.lon0()) / (viewport.gpts.lon1() - viewport.gpts.lon0());
        let ty = (lat - viewport.gpts.lat0()) / (viewport.gpts.lat1() - viewport.gpts.lat0());
        let x = viewport.pdf.x0() + tx * (viewport.pdf.x1() - viewport.pdf.x0());
        let y = viewport.pdf.y0() + ty * (viewport.pdf.y1() - viewport.pdf.y0());
        Ok((x, y))
    }

    /// Map a `/LPTS` local point to longitude/latitude of the first viewport.
    pub fn local_to_geo(&self, x: f64, y: f64) -> Result<(f64, f64), GeospatialError> {
        self.check_projection()?;
        let local = self.lpts.as_ref().ok_or(GeospatialError::NoLocalPoints)?;
        let first = self.viewport(0)?;
        let tx = (x - local.x0()) / (local.x1() - local.x0());
        let ty = (y - local.y0()) / (local.y1() - local.y0());
        let lon = first.gpts.lon0() + tx * (first.gpts.lon1() - first.gpts.lon0());
        let lat = first.gpts.lat0() + ty * (first.gpts.lat1() - first.gpts.lat0());
        Ok((lon, lat))
    }

    fn viewport(&self, index: usize) -> Result<&Viewport, GeospatialError> {
        self.viewports
            .get(index)
            .ok_or(GeospatialError::ViewportIndexOutOfRange {
                index,
                count: self.viewports.len(),
            })
    }

    /// Produce a deterministic plan describing what a backend must do.
    pub fn resolve_plan(&self) -> GeospatialPlan {
        let mut notes = Vec::new();
        if !self.gcs.projection().is_supported() {
            notes.push(PlanNote::UnsupportedProjection(
                self.gcs.projection().unsupported_label().to_owned(),
            ));
        }
        if self
            .viewports
            .iter()
            .any(|vp| !vp.measure.unit().is_linear() && !vp.measure.unit().is_angular())
        {
            notes.push(PlanNote::UnknownUnit);
        }
        if self.lpts.is_none() {
            notes.push(PlanNote::NoLocalPoints);
        }
        GeospatialPlan {
            supported: self.gcs.projection().is_supported(),
            viewport_count: self.viewports.len(),
            projection: self.gcs.projection().clone(),
            unit: self.viewports[0].measure().unit().clone(),
            notes,
        }
    }
}

/// A note attached to a [`GeospatialPlan`] for honest reporting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanNote {
    /// A projection that this model cannot map.
    UnsupportedProjection(String),
    /// At least one viewport uses a unit this model does not recognise.
    UnknownUnit,
    /// The document declares no `/LPTS` dictionary.
    NoLocalPoints,
}

/// The deterministic plan a geospatial backend must realise.
#[derive(Clone, Debug, PartialEq)]
pub struct GeospatialPlan {
    supported: bool,
    viewport_count: usize,
    projection: Projection,
    unit: Unit,
    notes: Vec<PlanNote>,
}

impl GeospatialPlan {
    /// Return `true` when this model can map the document's coordinates.
    pub fn supported(&self) -> bool {
        self.supported
    }

    /// Return the number of viewports.
    pub fn viewport_count(&self) -> usize {
        self.viewport_count
    }

    /// Return the declared projection.
    pub fn projection(&self) -> &Projection {
        &self.projection
    }

    /// Return the unit of the first viewport.
    pub fn unit(&self) -> &Unit {
        &self.unit
    }

    /// Return reporting notes in stable order.
    pub fn notes(&self) -> &[PlanNote] {
        &self.notes
    }
}

impl Projection {
    fn unsupported_label(&self) -> &str {
        match self {
            Self::Geographic => "Geographic",
            Self::WebMercator => "WebMercator",
            Self::Unsupported(name) => name.as_str(),
        }
    }
}

fn validate_coordinate(field: &'static str, value: f64) -> Result<(), GeospatialError> {
    if !value.is_finite() {
        return Err(GeospatialError::NonFiniteCoordinate { field, value });
    }
    if value.abs() > MAX_COORD_ABS {
        return Err(GeospatialError::CoordinateOutOfRange {
            field,
            value,
            limit: MAX_COORD_ABS,
        });
    }
    Ok(())
}

#[cfg(test)]
fn sample_dictionary() -> GeospatialDictionary {
    GeospatialDictionary {
        viewports: vec![ViewportInput {
            pdf_rect: [0.0, 0.0, 100.0, 200.0],
            gpts: [-122.4, 37.7, -122.3, 37.8],
            measure_subtype: "Linear".to_owned(),
            x_step: 1.0,
            y_step: 1.0,
            measure_unit: "m".to_owned(),
        }],
        gcs_name: "WGS84".to_owned(),
        gcs_epsg: Some(4326),
        lpts: Some([0.0, 0.0, 1000.0, 2000.0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1.0e-9
    }

    #[test]
    fn parse_viewport_arrays() {
        let doc = GeospatialDocument::parse(sample_dictionary()).expect("valid document");
        assert_eq!(doc.viewports().len(), 1);
        let vp = &doc.viewports()[0];
        assert_eq!(vp.pdf().x0(), 0.0);
        assert_eq!(vp.pdf().x1(), 100.0);
        assert_eq!(vp.pdf().y1(), 200.0);
        assert_eq!(vp.gpts().lon0(), -122.4);
        assert_eq!(vp.gpts().lat1(), 37.8);
        assert_eq!(vp.measure().unit(), &Unit::Metre);
    }

    #[test]
    fn viewport_bounds_validation() {
        let mut dict = sample_dictionary();
        dict.viewports[0].pdf_rect = [10.0, 0.0, 0.0, 200.0];
        assert_eq!(
            GeospatialDocument::parse(dict),
            Err(GeospatialError::InvalidViewportRectangle)
        );

        let mut dict = sample_dictionary();
        dict.viewports[0].pdf_rect = [0.0, 0.0, f64::NAN, 200.0];
        assert!(matches!(
            GeospatialDocument::parse(dict),
            Err(GeospatialError::NonFiniteCoordinate { field: "x1", value }) if value.is_nan()
        ));

        let mut dict = sample_dictionary();
        dict.viewports[0].pdf_rect = [0.0, 0.0, MAX_COORD_ABS + 1.0, 200.0];
        assert!(matches!(
            GeospatialDocument::parse(dict),
            Err(GeospatialError::CoordinateOutOfRange { .. })
        ));
    }

    #[test]
    fn gpts_and_gcs_parse() {
        let doc = GeospatialDocument::parse(sample_dictionary()).expect("valid document");
        assert_eq!(doc.gcs().name(), "WGS84");
        assert_eq!(doc.gcs().epsg(), Some(4326));
        assert_eq!(doc.gcs().projection(), &Projection::Geographic);
        assert!(doc.lpts().is_some());

        let mut dict = sample_dictionary();
        dict.viewports[0].gpts = [-122.4, 95.0, -122.3, 37.8];
        assert_eq!(
            GeospatialDocument::parse(dict),
            Err(GeospatialError::InvalidGeoBounds)
        );
    }

    #[test]
    fn measure_scale_conversions() {
        let mut dict = sample_dictionary();
        dict.viewports[0].measure_unit = "km".to_owned();
        let doc = GeospatialDocument::parse(dict).expect("kilometre measure");
        let measure = &doc.viewports()[0].measure();
        assert!(approx(
            measure.x_step_base().expect("linear metre factor"),
            1000.0
        ));

        let mut dict = sample_dictionary();
        dict.viewports[0].measure_unit = "ft".to_owned();
        let doc = GeospatialDocument::parse(dict).expect("foot measure");
        let base = doc.viewports()[0]
            .measure()
            .x_step_base()
            .expect("linear metre factor");
        assert!(approx(base, 0.304_8));

        let mut dict = sample_dictionary();
        dict.viewports[0].measure_subtype = "Angular".to_owned();
        dict.viewports[0].measure_unit = "deg".to_owned();
        let doc = GeospatialDocument::parse(dict).expect("degree measure");
        let base = doc.viewports()[0]
            .measure()
            .x_step_base()
            .expect("angular radian factor");
        assert!(approx(base, std::f64::consts::PI / 180.0));

        let mut dict = sample_dictionary();
        dict.viewports[0].measure_subtype = "Angular".to_owned();
        let doc = GeospatialDocument::parse(dict).expect("mismatched measure");
        assert_eq!(doc.viewports()[0].measure().x_step_base(), None);
    }

    #[test]
    fn coordinate_mapping_is_deterministic() {
        let doc = GeospatialDocument::parse(sample_dictionary()).expect("valid");
        let first = doc.pdf_to_geo(0, 50.0, 100.0).expect("centre maps");
        let second = doc.pdf_to_geo(0, 50.0, 100.0).expect("centre maps again");
        assert_eq!(first, second);
        assert!(approx(first.0, -122.35));
        assert!(approx(first.1, 37.75));

        let corners = doc.pdf_to_geo(0, 0.0, 0.0).expect("corner maps");
        assert_eq!(corners, (-122.4, 37.7));
        let far = doc.pdf_to_geo(0, 100.0, 200.0).expect("corner maps");
        assert!(approx(far.0, -122.3));
        assert!(approx(far.1, 37.8));
    }

    #[test]
    fn inverse_mapping_round_trips() {
        let doc = GeospatialDocument::parse(sample_dictionary()).expect("valid");
        let (lon, lat) = doc.pdf_to_geo(0, 25.0, 175.0).expect("forward");
        let (x, y) = doc.geo_to_pdf(0, lon, lat).expect("inverse");
        assert!(approx(x, 25.0));
        assert!(approx(y, 175.0));
    }

    #[test]
    fn point_outside_viewport_is_rejected() {
        let doc = GeospatialDocument::parse(sample_dictionary()).expect("valid");
        assert!(matches!(
            doc.pdf_to_geo(0, -1.0, 100.0),
            Err(GeospatialError::PointOutsideViewport { .. })
        ));
        assert!(matches!(
            doc.geo_to_pdf(0, -180.0, 37.75),
            Err(GeospatialError::PointOutsideViewport { .. })
        ));
        assert_eq!(
            doc.pdf_to_geo(3, 0.0, 0.0),
            Err(GeospatialError::ViewportIndexOutOfRange { index: 3, count: 1 })
        );
    }

    #[test]
    fn lpts_local_mapping() {
        let doc = GeospatialDocument::parse(sample_dictionary()).expect("valid");
        let (lon, lat) = doc.local_to_geo(500.0, 1000.0).expect("local midpoint");
        assert!(approx(lon, -122.35));
        assert!(approx(lat, 37.75));

        let mut dict = sample_dictionary();
        dict.lpts = None;
        let doc = GeospatialDocument::parse(dict).expect("no lpts");
        assert_eq!(
            doc.local_to_geo(0.0, 0.0),
            Err(GeospatialError::NoLocalPoints)
        );
    }

    #[test]
    fn unsupported_projection_is_honest() {
        let mut dict = sample_dictionary();
        dict.gcs_name = "EPSG:3857".to_owned();
        let doc = GeospatialDocument::parse(dict).expect("web mercator gcs");
        assert_eq!(doc.gcs().projection(), &Projection::WebMercator);
        assert_eq!(
            doc.pdf_to_geo(0, 0.0, 0.0),
            Err(GeospatialError::UnsupportedProjection(
                "WebMercator".to_owned()
            ))
        );
        let plan = doc.resolve_plan();
        assert!(!plan.supported());
        assert!(
            plan.notes()
                .contains(&PlanNote::UnsupportedProjection("WebMercator".to_owned()))
        );

        let mut dict = sample_dictionary();
        dict.gcs_name = "Albers Equal Area".to_owned();
        let doc = GeospatialDocument::parse(dict).expect("unknown projection gcs");
        assert!(matches!(doc.gcs().projection(), Projection::Unsupported(_)));
        assert!(matches!(
            doc.pdf_to_geo(0, 0.0, 0.0),
            Err(GeospatialError::UnsupportedProjection(_))
        ));
    }

    #[test]
    fn plan_is_deterministic_and_reports_notes() {
        let doc = GeospatialDocument::parse(sample_dictionary()).expect("valid");
        assert_eq!(doc.resolve_plan(), doc.resolve_plan());
        let plan = doc.resolve_plan();
        assert!(plan.supported());
        assert_eq!(plan.viewport_count(), 1);
        assert_eq!(plan.unit(), &Unit::Metre);
        // The sample document has LPTS and a geographic, known-unit measure.
        assert!(plan.notes().is_empty());

        let mut dict = sample_dictionary();
        dict.lpts = None;
        let no_lpts = GeospatialDocument::parse(dict)
            .expect("valid")
            .resolve_plan();
        assert_eq!(no_lpts.notes(), &[PlanNote::NoLocalPoints]);
    }

    #[test]
    fn invalid_input_is_rejected() {
        let mut dict = sample_dictionary();
        dict.viewports.clear();
        assert_eq!(
            GeospatialDocument::parse(dict),
            Err(GeospatialError::EmptyViewports)
        );

        let mut dict = sample_dictionary();
        dict.viewports = vec![sample_dictionary().viewports[0].clone(); MAX_VIEWPORTS + 1];
        assert_eq!(
            GeospatialDocument::parse(dict),
            Err(GeospatialError::TooManyViewports {
                count: MAX_VIEWPORTS + 1,
                limit: MAX_VIEWPORTS
            })
        );

        let mut dict = sample_dictionary();
        dict.viewports[0].x_step = 0.0;
        assert!(matches!(
            GeospatialDocument::parse(dict),
            Err(GeospatialError::InvalidMeasureStep { .. })
        ));

        let mut dict = sample_dictionary();
        dict.gcs_name = String::new();
        assert_eq!(
            GeospatialDocument::parse(dict),
            Err(GeospatialError::InvalidGcsName {
                limit: MAX_NAME_BYTES
            })
        );

        let mut dict = sample_dictionary();
        dict.lpts = Some([0.0, 0.0, 0.0, 2000.0]);
        assert_eq!(
            GeospatialDocument::parse(dict),
            Err(GeospatialError::InvalidLocalPoints)
        );
    }

    #[test]
    fn unknown_unit_is_preserved_and_reported() {
        let mut dict = sample_dictionary();
        dict.viewports[0].measure_unit = "furlong".to_owned();
        let doc = GeospatialDocument::parse(dict).expect("unknown unit is kept");
        assert_eq!(
            doc.viewports()[0].measure().unit(),
            &Unit::Unknown("furlong".to_owned())
        );
        assert_eq!(doc.viewports()[0].measure().x_step_base(), None);
        assert!(doc.resolve_plan().notes().contains(&PlanNote::UnknownUnit));
    }
}
