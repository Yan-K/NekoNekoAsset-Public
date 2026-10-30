//! Local Windows reading fallback. No text leaves the device and no reading is
//! treated as an official creator spelling. Runtime objects stay on their STA.
// Windows documents 100 characters, but some hosts return no words from 21
// UTF-16 units and have crashed after longer calls; 20 read on every host seen.
#[cfg(target_os = "windows")]
const NATIVE_CHUNK_UNITS: usize = 16;

#[cfg(target_os = "windows")]
fn reading_chunks(input: &str) -> Vec<String> {
    // Stay below the native limit without splitting a surrogate pair.
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut units = 0;
    for c in input.chars() {
        if units + c.len_utf16() > NATIVE_CHUNK_UNITS {
            chunks.push(std::mem::take(&mut chunk));
            units = 0;
        }
        chunk.push(c);
        units += c.len_utf16();
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

#[cfg(target_os = "windows")]
fn complete_chunk_reading(input: &str, pieces: &[(String, String)]) -> Option<String> {
    let mut display = String::new();
    let mut output = String::new();
    for (source, reading) in pieces {
        if source.is_empty() || reading.is_empty() {
            return None;
        }
        display.push_str(source);
        output.push_str(reading);
    }
    (!display.is_empty() && display == input && !output.is_empty()).then_some(output)
}

#[cfg(target_os = "windows")]
fn complete_chunk_with_retry(
    input: &str,
    mut analyze: impl FnMut() -> Option<Vec<(String, String)>>,
) -> Option<String> {
    for _ in 0..3 {
        let pieces = analyze()?;
        if !pieces.is_empty() {
            return complete_chunk_reading(input, &pieces);
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn reading_cache() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, String>> {
    use std::{collections::BTreeMap, sync::{Mutex, OnceLock}};
    static CACHE: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(target_os = "windows")]
pub(super) fn reading(input: &str) -> Option<String> {
    let contains_han=input.chars().any(|c| matches!(c as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af));
    if !contains_han
        || input
            .chars()
            .any(|c| matches!(c as u32,0x1100..=0x11ff | 0xac00..=0xd7af))
    {
        return None;
    }
    // Serialize cache misses through the native call. Failed native calls are
    // retryable and cannot overwrite an already successful reading.
    let mut values = reading_cache().lock().ok()?;
    if let Some(found) = values.get(input).cloned() {
        return Some(found);
    }
    let owned = input.to_owned();
    let result = std::thread::Builder::new()
        .name("avatar-reading".into())
        .spawn(move || {
            use windows::{
                core::HSTRING,
                Globalization::JapanesePhoneticAnalyzer,
                Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_SINGLETHREADED},
            };
            unsafe {
                RoInitialize(RO_INIT_SINGLETHREADED).ok()?;
            }
            struct Apartment;
            impl Drop for Apartment {
                fn drop(&mut self) {
                    unsafe {
                        RoUninitialize();
                    }
                }
            }
            let _apartment = Apartment;
            let mut output = String::new();
            for chunk in reading_chunks(&owned) {
                let input = HSTRING::from(chunk.as_str());
                let reading = complete_chunk_with_retry(&chunk, || {
                    let words = JapanesePhoneticAnalyzer::GetWords(&input).ok()?;
                    let mut pieces = Vec::new();
                    for index in 0..words.Size().ok()? {
                        let word = words.GetAt(index).ok()?;
                        pieces.push((
                            word.DisplayText().ok()?.to_string_lossy(),
                            word.YomiText().ok()?.to_string_lossy(),
                        ));
                    }
                    Some(pieces)
                })?;
                output.push_str(&reading);
            }
            (!output.is_empty()).then_some(output)
        })
        .ok()?
        .join()
        .ok()
        .flatten();
    if let Some(result) = &result {
        if values.len() >= 2048 {
            values.clear();
        }
        values.insert(input.to_owned(), result.clone());
    }
    result
}

#[cfg(not(target_os = "windows"))]
pub(super) fn reading(_input: &str) -> Option<String> {
    None
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    #[test]
    fn local_windows_reading_handles_synthetic_kanji_and_bounded_input() {
        let reading = super::reading("星空猫");
        if reading.is_none() {
            diagnose_bounded_reading();
        }
        let reading = reading.expect("Windows local phonetic analyzer");
        assert_eq!(reading, "ほしぞらねこ");
        assert_eq!(super::reading("星空猫"), Some(reading));
        let input = "星空".repeat(60);
        let bounded = super::reading(&input);
        if bounded.is_none() {
            diagnose_bounded_reading();
        }
        let bounded = bounded.expect("Windows local phonetic analyzer bounded input");
        let chunks = super::reading_chunks(&input);
        assert_eq!(chunks.concat(), input);
        assert!(chunks.iter().all(|chunk| (1..=super::NATIVE_CHUNK_UNITS).contains(&chunk.encode_utf16().count())));
        let independently_read = chunks.iter().map(|chunk| super::reading(chunk))
            .collect::<Option<Vec<_>>>().expect("Windows local phonetic analyzer exact chunks");
        assert_eq!(bounded, independently_read.concat());
        assert_eq!(super::reading("Synthetic English"), None);
        assert_eq!(super::reading("合成한국어"), None);
    }

    #[test]
    fn reading_chunks_cover_every_scalar_below_the_native_boundary() {
        for input in [String::new(), "星".repeat(16), "星".repeat(17), "星".repeat(100), "星空".repeat(60),
            "星".repeat(15) + "\u{20000}" + "猫", "\u{20000}".repeat(100)] {
            let chunks = super::reading_chunks(&input);
            assert_eq!(chunks.concat(), input);
            assert!(chunks.iter().all(|chunk| (1..=super::NATIVE_CHUNK_UNITS).contains(&chunk.encode_utf16().count())));
        }
    }

    #[test]
    fn complete_chunk_reading_rejects_missing_empty_and_partial_source_coverage() {
        let pieces = [("星空".into(), "ほしぞら".into()), ("猫".into(), "ねこ".into())];
        assert_eq!(super::complete_chunk_reading("星空猫", &pieces).as_deref(), Some("ほしぞらねこ"));
        for pieces in [
            vec![], vec![("星空".into(), "ほしぞら".into())],
            vec![("星空猫".into(), "".into())], vec![("".into(), "ほしぞらねこ".into())],
            vec![("猫".into(), "ねこ".into()), ("星空".into(), "ほしぞら".into())],
            vec![("星空猫".into(), "ほしぞらねこ".into()), ("猫".into(), "ねこ".into())],
        ] {
            assert_eq!(super::complete_chunk_reading("星空猫", &pieces), None);
        }
        assert_eq!(super::complete_chunk_reading("", &[]), None);
    }

    #[test]
    fn native_empty_recovery_is_bounded_and_never_retries_partial_results() {
        let mut calls = 0;
        let reading = super::complete_chunk_with_retry("星空猫", || {
            calls += 1;
            Some(if calls == 1 { vec![] } else { vec![("星空猫".into(), "ほしぞらねこ".into())] })
        });
        assert_eq!(reading.as_deref(), Some("ほしぞらねこ"));
        assert_eq!(calls, 2);
        let mut calls = 0;
        assert_eq!(super::complete_chunk_with_retry("星空猫", || {
            calls += 1;
            Some(vec![])
        }), None);
        assert_eq!(calls, 3);
        for result in [
            None,
            Some(vec![("星空".into(), "ほしぞら".into())]),
            Some(vec![("星空猫".into(), "".into())]),
        ] {
            let mut calls = 0;
            assert_eq!(super::complete_chunk_with_retry("星空猫", || {
                calls += 1;
                result.clone()
            }), None);
            assert_eq!(calls, 1);
        }
    }


    // Only authored synthetic input reaches this probe. Report native API stages
    // and HRESULTs without adding catalogue text to test output.
    fn diagnose_bounded_reading() {
        // Failure diagnostics must not overlap another test's normal native
        // call; use the same exclusion without invoking or changing the cache.
        let Ok(_native_exclusion) = super::reading_cache().lock() else {
            eprintln!("authored phonetic diagnostic stage=cache-lock outcome=poisoned");
            return;
        };
        use windows::{
            core::{Error, HSTRING},
            Globalization::JapanesePhoneticAnalyzer,
            Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_SINGLETHREADED},
        };
        fn native_error(stage: &str, error: Error, chunk: usize, units: usize) -> String {
            format!(
                "stage={stage} hresult=0x{:08X} chunk={chunk} utf16Units={units}",
                error.code().0 as u32
            )
        }
        // Longer probes are left out: near-100 calls preceded hosted crashes.
        let cases = [
            ("short-synthetic", vec!["星空猫".into()]),
            ("punctuated-safe-split-120", super::reading_chunks(&"星空を見て猫と歩く。".repeat(12))),
            ("kanji-safe-split-120", super::reading_chunks(&"星空".repeat(60))),
        ];
        for (case, chunks) in cases {
            let unit_counts: Vec<_> = chunks.iter().map(|input| input.encode_utf16().count()).collect();
            let outcome = std::thread::spawn(move || -> Result<(usize, usize), String> {
                unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }
                    .map_err(|e| native_error("RoInitialize", e, 0, 0))?;
                struct Apartment;
                impl Drop for Apartment {
                    fn drop(&mut self) {
                        unsafe { RoUninitialize() };
                    }
                }
                let _apartment = Apartment;
                let mut word_count = 0;
                let mut output_units = 0;
                for (chunk, input) in chunks.into_iter().enumerate() {
                    let units = input.encode_utf16().count();
                    let words = JapanesePhoneticAnalyzer::GetWords(&HSTRING::from(input))
                        .map_err(|e| native_error("GetWords", e, chunk, units))?;
                    let size = words
                        .Size()
                        .map_err(|e| native_error("Size", e, chunk, units))?;
                    word_count += size as usize;
                    for index in 0..size {
                        let word = words
                            .GetAt(index)
                            .map_err(|e| native_error("GetAt", e, chunk, units))?;
                        let reading = word
                            .YomiText()
                            .map_err(|e| native_error("YomiText", e, chunk, units))?;
                        output_units += reading.len();
                    }
                }
                Ok((word_count, output_units))
            })
            .join();
            eprintln!("authored phonetic diagnostic case={case} utf16Units={unit_counts:?} outcome={outcome:?}");
        }
    }
}
