//! Local Windows reading fallback. No text leaves the device and no reading is
//! treated as an official creator spelling. Runtime objects stay on their STA.
#[cfg(target_os = "windows")]
pub(super) fn reading(input: &str) -> Option<String> {
    use std::{
        collections::BTreeMap,
        sync::{Mutex, OnceLock},
    };
    static CACHE: OnceLock<Mutex<BTreeMap<String, Option<String>>>> = OnceLock::new();
    let contains_han=input.chars().any(|c| matches!(c as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af));
    if !contains_han
        || input
            .chars()
            .any(|c| matches!(c as u32,0x1100..=0x11ff | 0xac00..=0xd7af))
    {
        return None;
    }
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(found) = cache.lock().ok()?.get(input).cloned() {
        return found;
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
            let mut chunks = Vec::new();
            let mut chunk = String::new();
            let mut units = 0;
            for c in owned.chars() {
                if units + c.len_utf16() > 100 {
                    chunks.push(std::mem::take(&mut chunk));
                    units = 0;
                }
                chunk.push(c);
                units += c.len_utf16();
            }
            if !chunk.is_empty() {
                chunks.push(chunk);
            }
            let mut output = String::new();
            for chunk in chunks {
                let words = JapanesePhoneticAnalyzer::GetWords(&HSTRING::from(chunk)).ok()?;
                for index in 0..words.Size().ok()? {
                    output.push_str(&words.GetAt(index).ok()?.YomiText().ok()?.to_string_lossy());
                }
            }
            (!output.is_empty()).then_some(output)
        })
        .ok()?
        .join()
        .ok()
        .flatten();
    if let Ok(mut values) = cache.lock() {
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
        let reading = super::reading("星空猫").expect("Windows local phonetic analyzer");
        assert_eq!(reading, "ほしぞらねこ");
        assert_eq!(super::reading("星空猫"), Some(reading));
        let bounded = super::reading(&"星空".repeat(60));
        if bounded.is_none() {
            diagnose_bounded_reading();
        }
        assert!(bounded.is_some());
        assert_eq!(super::reading("Synthetic English"), None);
        assert_eq!(super::reading("合成한국어"), None);
    }


    // Only authored synthetic input reaches this probe. Report native API stages
    // and HRESULTs without adding catalogue text to test output.
    fn diagnose_bounded_reading() {
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
        let cases = [
            ("direct-99", vec!["星空".repeat(49) + "星"]),
            ("direct-100", vec!["星空".repeat(50)]),
            ("direct-101", vec!["星空".repeat(50) + "星"]),
            ("punctuated-100", vec!["星空を見て猫と歩く。".repeat(10)]),
            (
                "punctuated-split-120",
                vec!["星空を見て猫と歩く。".repeat(10), "星空を見て猫と歩く。".repeat(2)],
            ),
            ("kanji-split-120", vec!["星空".repeat(50), "星空".repeat(10)]),
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
