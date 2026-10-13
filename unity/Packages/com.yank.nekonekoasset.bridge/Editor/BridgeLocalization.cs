using System;
using System.Collections.Generic;

namespace NekoNekoAsset.Bridge
{
    /// <summary>Labels for the native review UI; the receiver selects its system/editor locale.</summary>
    public sealed class BridgeLocalization
    {
        private readonly int language;

        public BridgeLocalization(string locale)
        {
            switch (locale)
            {
                case "ja": language = 1; break;
                case "ko": language = 2; break;
                case "zh-Hans": language = 3; break;
                case "zh-Hant": language = 4; break;
                default: language = 0; break;
            }
        }

        public string Text(string key)
        {
            return Labels.TryGetValue(key, out string[] labels) ? labels[language] : key;
        }

        public string Format(string key, params object[] values)
        {
            return string.Format(Text(key), values);
        }

        private static readonly Dictionary<string, string[]> Labels = new Dictionary<string, string[]>(StringComparer.Ordinal)
        {
            ["resultTitle"] = new[] { "Import results", "インポート結果", "가져오기 결과", "导入结果", "匯入結果" },
            ["completed"] = new[] { "Completed", "完了", "완료", "已完成", "已完成" },
            ["failed"] = new[] { "Failed", "失敗", "실패", "失败", "失敗" },
            ["applied"] = new[] { "Applied", "適用済み", "적용됨", "已应用", "已套用" },
            ["skipped"] = new[] { "Skipped", "スキップ", "건너뜀", "已跳过", "已略過" },
            ["close"] = new[] { "Close", "閉じる", "닫기", "关闭", "關閉" },
            ["resultCounts"] = new[] { "Applied: {0}  Skipped: {1}  Failed: {2}", "適用: {0}  スキップ: {1}  失敗: {2}", "적용: {0}  건너뜀: {1}  실패: {2}", "已应用: {0}  已跳过: {1}  失败: {2}", "已套用: {0}  已略過: {1}  失敗: {2}" },
            ["cleanup"] = new[] { "Clean up new files", "新規ファイルを削除", "새 파일 정리", "清理新文件", "清理新增檔案" },
            ["cleanupConfirm"] = new[] { "Remove this import's unchanged new files? Preexisting and changed files are kept.", "今回の未変更の新規ファイルを削除しますか？既存・変更済みファイルは残ります。", "이번에 추가된 변경되지 않은 파일을 삭제할까요? 기존 파일과 수정된 파일은 유지됩니다.", "删除本次导入后未修改的新文件？保留原有和已修改的文件。", "刪除本次匯入後未修改的新增檔案？保留原有與已修改的檔案。" },
            ["cleanupCounts"] = new[] { "Removed: {0}  Retained: {1}", "削除: {0}  保持: {1}", "삭제: {0}  유지: {1}", "已删除: {0}  已保留: {1}", "已刪除: {0}  已保留: {1}" },
            ["title"] = new[] { "NekoNekoAsset Import", "NekoNekoAsset インポート", "NekoNekoAsset 가져오기", "NekoNekoAsset 导入", "NekoNekoAsset 匯入" },
            ["importing"] = new[] { "Importing", "インポート中", "가져오는 중", "正在导入", "正在匯入" },
            ["checkingTargets"] = new[] { "Checking targets…", "保存先を確認中…", "대상 확인 중…", "正在检查目标…", "正在檢查目標…" },
            ["import"] = new[] { "Import", "インポート", "가져오기", "导入", "匯入" },
            ["cancel"] = new[] { "Cancel", "キャンセル", "취소", "取消", "取消" },
            ["selectAll"] = new[] { "Select All", "すべて選択", "모두 선택", "全选", "全選" },
            ["selectNone"] = new[] { "Select None", "選択を解除", "선택 해제", "全不选", "全不選" },
            ["expandAll"] = new[] { "Expand All", "すべて展開", "모두 펼치기", "全部展开", "全部展開" },
            ["foldAll"] = new[] { "Collapse All", "すべて折りたたむ", "모두 접기", "全部折叠", "全部摺疊" },
            ["conflictPolicy"] = new[] { "Conflicts", "競合", "충돌", "冲突", "衝突" },
            ["ask"] = new[] { "Ask", "確認", "확인", "询问", "詢問" },
            ["overwrite"] = new[] { "Overwrite", "上書き", "덮어쓰기", "覆盖", "覆寫" },
            ["skip"] = new[] { "Skip", "スキップ", "건너뛰기", "跳过", "略過" },
            ["overwriteAll"] = new[] { "Overwrite All", "すべて上書き", "모두 덮어쓰기", "全部覆盖", "全部覆寫" },
            ["skipAll"] = new[] { "Skip All", "すべてスキップ", "모두 건너뛰기", "全部跳过", "全部略過" },
            ["new"] = new[] { "New", "新規", "새 파일", "新增", "新增" },
            ["identical"] = new[] { "Identical", "同一", "동일", "相同", "相同" },
            ["guidConflict"] = new[] { "GUID Conflict", "GUID 競合", "GUID 충돌", "GUID 冲突", "GUID 衝突" },
            ["conflictCount"] = new[] { "{0} GUID Conflict", "{0} GUID 競合", "GUID 충돌 {0}", "{0} GUID 冲突", "{0} GUID 衝突" },
            ["identicalCount"] = new[] { "{0} Identical", "{0} 同一", "동일 {0}", "{0} 相同", "{0} 相同" },
            ["selectedCount"] = new[] { "{0} / {1} selected", "{0} / {1} 選択", "{0} / {1} 선택", "已选 {0} / {1}", "已選 {0} / {1}" },
            ["destination"] = new[] { "Destination", "保存先", "대상 폴더", "目标路径", "目標路徑" },
            ["choose"] = new[] { "Choose…", "選択…", "선택…", "选择…", "選擇…" },
            ["chooseDestination"] = new[] { "Choose Assets folder", "Assets 内のフォルダーを選択", "Assets 폴더 선택", "选择 Assets 文件夹", "選擇 Assets 資料夾" },
            ["invalidDestination"] = new[] { "Choose a path inside Assets.", "Assets 内のパスを選択してください。", "Assets 안의 경로를 선택하세요.", "请选择 Assets 内的路径。", "請選擇 Assets 內的路徑。" },
            ["unified"] = new[] { "Unified location", "共通の保存先", "통합 위치", "统一位置", "統一位置" },
            ["individual"] = new[] { "Individual locations", "個別の保存先", "개별 위치", "独立位置", "獨立位置" },
            ["looseFiles"] = new[] { "Files", "ファイル", "파일", "文件", "檔案" },
            ["empty"] = new[] { "No files selected", "ファイルが選択されていません", "선택한 파일 없음", "未选择文件", "未選擇檔案" },
            ["invalidInput"] = new[] { "The import could not be prepared.", "インポートを準備できませんでした。", "가져오기를 준비할 수 없습니다.", "无法准备导入。", "無法準備匯入。" },
            ["planChanged"] = new[] { "The import targets changed. Review the updated selection.", "インポート先が変わりました。更新された選択を確認してください。", "가져올 대상이 변경되었습니다. 선택을 다시 확인하세요.", "导入目标已改变，请检查更新后的选择。", "匯入目標已改變，請檢查更新後的選擇。" },
            ["receiptTooLarge"] = new[] { "The result exceeds the transfer limit. Select fewer files.", "結果が転送サイズの上限を超えます。選択するファイルを減らしてください。", "결과가 전송 크기 제한을 초과합니다. 선택한 파일 수를 줄이세요.", "结果超过传输大小上限，请减少所选文件。", "結果超過傳輸大小上限，請減少選取的檔案。" },
            ["conflictTitle"] = new[] { "NekoNekoAsset Conflict", "NekoNekoAsset 競合", "NekoNekoAsset 충돌", "NekoNekoAsset 冲突", "NekoNekoAsset 衝突" },
            ["conflictHeading"] = new[] { "Import conflict", "インポートの競合", "가져오기 충돌", "导入冲突", "匯入衝突" },
            ["thisConflict"] = new[] { "This conflict", "この競合", "이 충돌", "此冲突", "此衝突" },
            ["remainingConflicts"] = new[] { "This and all remaining conflicts", "この競合と残りのすべての競合", "이 충돌과 남은 모든 충돌", "此冲突及后续所有冲突", "此衝突及後續所有衝突" },
            ["pathManaged"] = new[] { "(path-managed)", "(パス管理)", "(경로 관리)", "（按路径管理）", "（按路徑管理）" },
            ["none"] = new[] { "(none)", "(なし)", "(없음)", "（无）", "（無）" },
            ["conflictDetails"] = new[]
            {
                "Kind: {0}\nIncoming: {1}\nIncoming GUID: {2}\nTarget: {3}\nExisting GUID: {4}",
                "種類: {0}\n受信: {1}\n受信 GUID: {2}\n保存先: {3}\n既存 GUID: {4}",
                "종류: {0}\n받는 경로: {1}\n받는 GUID: {2}\n대상: {3}\n기존 GUID: {4}",
                "类型：{0}\n传入路径：{1}\n传入 GUID：{2}\n目标：{3}\n现有 GUID：{4}",
                "類型：{0}\n傳入路徑：{1}\n傳入 GUID：{2}\n目標：{3}\n現有 GUID：{4}"
            }
        };
    }
}
