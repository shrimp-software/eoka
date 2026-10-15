use super::{Page, PageState};
use crate::error::Result;

/// Encoding used for a screenshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenshotFormat {
    /// Lossless PNG (the default).
    #[default]
    Png,
    /// JPEG, with optional quality from 0 through 100.
    Jpeg,
    /// WebP, with optional quality from 0 through 100.
    Webp,
}

/// A document-relative crop in CSS pixels, independent of device pixel ratio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenshotClip {
    /// Nonnegative horizontal document position.
    pub x: f64,
    /// Nonnegative vertical document position.
    pub y: f64,
    /// Positive crop width.
    pub width: f64,
    /// Positive crop height.
    pub height: f64,
}

/// Screenshot encoding and extent. The default captures the visible viewport as PNG.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScreenshotOptions {
    /// Image encoding.
    pub format: ScreenshotFormat,
    /// JPEG/WebP quality from 0 through 100; invalid for PNG.
    pub quality: Option<u8>,
    /// Document-relative CSS crop, mutually exclusive with `full_page`.
    pub clip: Option<ScreenshotClip>,
    /// Capture the complete CSS document content rather than the visible viewport.
    pub full_page: bool,
}

impl ScreenshotOptions {
    pub(crate) fn validate(&self) -> Result<()> {
        let invalid = |message: &str| crate::Error::InvalidScreenshotOptions(message.into());
        if let Some(quality) = self.quality {
            if quality > 100 {
                return Err(invalid("quality must be between 0 and 100"));
            }
            if self.format == ScreenshotFormat::Png {
                return Err(invalid("PNG does not accept quality"));
            }
        }
        if self.full_page && self.clip.is_some() {
            return Err(invalid("clip and full_page are mutually exclusive"));
        }
        if let Some(clip) = self.clip {
            if !clip.x.is_finite() || !clip.y.is_finite() || clip.x < 0.0 || clip.y < 0.0 {
                return Err(invalid("clip positions must be finite and nonnegative"));
            }
            if !clip.width.is_finite()
                || !clip.height.is_finite()
                || clip.width <= 0.0
                || clip.height <= 0.0
            {
                return Err(invalid("clip dimensions must be finite and positive"));
            }
        }
        Ok(())
    }
}

impl Page {
    /// Get page title
    pub async fn title(&self) -> Result<String> {
        // Use evaluate_sync (awaitPromise=false) so heavy SPAs don't block
        self.evaluate_sync("document.title || ''").await
    }

    /// Get page HTML content
    pub async fn content(&self) -> Result<String> {
        self.evaluate_sync("document.documentElement.outerHTML")
            .await
    }

    /// Get page text content (body innerText)
    pub async fn text(&self) -> Result<String> {
        self.evaluate_sync("document.body?.innerText || ''").await
    }

    /// Capture the visible viewport as PNG bytes at its current device pixel ratio.
    pub async fn screenshot(&self) -> Result<Vec<u8>> {
        self.screenshot_with_options(&ScreenshotOptions::default())
            .await
    }

    /// Capture the visible viewport as JPEG, with quality from 0 through 100.
    pub async fn screenshot_jpeg(&self, quality: u8) -> Result<Vec<u8>> {
        self.screenshot_with_options(&ScreenshotOptions {
            format: ScreenshotFormat::Jpeg,
            quality: Some(quality),
            ..Default::default()
        })
        .await
    }

    /// Capture PNG, JPEG or WebP with an optional CSS document crop or full-page extent.
    ///
    /// Only the viewport device pixel ratio scales the output; no second render
    /// scale is applied. Capture shares the tab's viewport coordinator and preserves
    /// emulated metrics and scrolling. Invalid options fail before any CDP command.
    pub async fn screenshot_with_options(&self, options: &ScreenshotOptions) -> Result<Vec<u8>> {
        self.session.capture_screenshot(options).await
    }

    /// Take a debug screenshot and save it with a timestamp
    ///
    /// Saves to `StealthConfig::debug_dir` if set, otherwise current directory.
    /// Useful during development to understand page state.
    pub async fn debug_screenshot(&self, prefix: &str) -> Result<String> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let filename = match &self.config.debug_dir {
            Some(dir) => {
                // Ensure directory exists
                std::fs::create_dir_all(dir)?;
                format!("{}/{}_{}.png", dir, prefix, timestamp)
            }
            None => format!("{}_{}.png", prefix, timestamp),
        };

        let screenshot = self.screenshot().await?;
        std::fs::write(&filename, screenshot)?;
        Ok(filename)
    }

    /// Log the current page state for debugging
    pub async fn debug_state(&self) -> Result<PageState> {
        let state: PageState = self
            .evaluate(
                r#"({
                url: location.href,
                title: document.title,
                input_count: document.querySelectorAll('input').length,
                button_count: document.querySelectorAll('button').length,
                link_count: document.querySelectorAll('a').length,
                form_count: document.querySelectorAll('form').length
            })"#,
            )
            .await
            .unwrap_or_else(|_| PageState {
                url: "unknown".to_string(),
                title: "unknown".to_string(),
                input_count: 0,
                button_count: 0,
                link_count: 0,
                form_count: 0,
            });
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::{ScreenshotClip, ScreenshotFormat, ScreenshotOptions};

    #[test]
    fn screenshot_quality_boundaries_and_encoding_combinations() {
        for format in [ScreenshotFormat::Jpeg, ScreenshotFormat::Webp] {
            for quality in [None, Some(0), Some(100)] {
                assert!(ScreenshotOptions {
                    format,
                    quality,
                    ..Default::default()
                }
                .validate()
                .is_ok());
            }
            for quality in [101, u8::MAX] {
                let error = ScreenshotOptions {
                    format,
                    quality: Some(quality),
                    ..Default::default()
                }
                .validate()
                .unwrap_err();
                assert!(matches!(error, crate::Error::InvalidScreenshotOptions(_)));
            }
        }
        for quality in [0, 80, 100] {
            assert!(ScreenshotOptions {
                quality: Some(quality),
                ..Default::default()
            }
            .validate()
            .is_err());
        }
    }

    #[test]
    fn screenshot_clip_rejects_invalid_geometry_and_full_page_conflicts() {
        let valid = ScreenshotClip {
            x: 0.0,
            y: 0.5,
            width: 0.5,
            height: 1.0,
        };
        assert!(ScreenshotOptions {
            clip: Some(valid),
            ..Default::default()
        }
        .validate()
        .is_ok());
        assert!(ScreenshotOptions {
            full_page: true,
            ..Default::default()
        }
        .validate()
        .is_ok());
        assert!(ScreenshotOptions {
            clip: Some(valid),
            full_page: true,
            ..Default::default()
        }
        .validate()
        .is_err());
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.5] {
            for clip in [
                ScreenshotClip { x: value, ..valid },
                ScreenshotClip { y: value, ..valid },
                ScreenshotClip {
                    width: value,
                    ..valid
                },
                ScreenshotClip {
                    height: value,
                    ..valid
                },
            ] {
                assert!(
                    ScreenshotOptions {
                        clip: Some(clip),
                        ..Default::default()
                    }
                    .validate()
                    .is_err(),
                    "accepted {clip:?}"
                );
            }
        }
        for clip in [
            ScreenshotClip {
                width: 0.0,
                ..valid
            },
            ScreenshotClip {
                height: 0.0,
                ..valid
            },
        ] {
            assert!(ScreenshotOptions {
                clip: Some(clip),
                ..Default::default()
            }
            .validate()
            .is_err());
        }
    }
}
