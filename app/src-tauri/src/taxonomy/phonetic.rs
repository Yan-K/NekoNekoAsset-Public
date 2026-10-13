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
        assert!(super::reading(&"星空".repeat(60)).is_some());
        assert_eq!(super::reading("Synthetic English"), None);
        assert_eq!(super::reading("合成한국어"), None);
    }
}
