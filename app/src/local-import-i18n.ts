import {nativeWorkflowText} from './native-workflow-i18n';
import type {Locale} from './model';

const words:Record<string,string[]>={
 dropFiles:['Drag files or folders here','ファイル・フォルダーをここにドロップ','파일이나 폴더를 여기에 놓으세요','拖放文件或文件夹到这里','拖放檔案或資料夾到這裡'],
 fetch:['Fetch','取得','가져오기','抓取','抓取'],

 singleProduct:['One product','1つの商品','상품 하나','一个商品','一個商品'],
 batchProducts:['Each folder / ZIP as a product','フォルダー／ZIPごとに商品','폴더 / ZIP별 상품','每个文件夹 / ZIP 为一个商品','每個資料夾 / ZIP 為一個商品'],
 addFiles:['Add files','ファイルを追加','파일 추가','添加文件','新增檔案'],
 addFolder:['Add folder','フォルダーを追加','폴더 추가','添加文件夹','新增資料夾'],
 importQueued:['Import queued','インポートを待機列に追加しました','가져오기를 대기열에 추가했습니다','已加入导入队列','已加入匯入佇列'],
 importSourcesMissing:['Choose local files or folders','ローカルのファイルまたはフォルダーを選択してください','로컬 파일 또는 폴더를 선택하세요','请选择本地文件或文件夹','請選擇本機檔案或資料夾'],
 no_import_files:['No files in the selected sources','選択した場所にファイルがありません','선택한 위치에 파일이 없습니다','所选来源中没有文件','所選來源中沒有檔案'],
 local_batch_requires_folder_or_zip:['Batch import accepts folders and ZIP files','一括インポートはフォルダーとZIPに対応しています','일괄 가져오기는 폴더와 ZIP 파일을 지원합니다','批量导入仅接受文件夹和 ZIP','批次匯入僅接受資料夾與 ZIP'],
 local_source_overlap:['Choose a folder or its contents, not both','フォルダーとその中身を重複して選択しないでください','폴더와 그 안의 파일을 중복 선택하지 마세요','请勿同时选择文件夹及其内容','請勿同時選擇資料夾與其內容'],
 local_source_contains_library:['The selected folder contains the asset library','選択したフォルダーにライブラリが含まれています','선택한 폴더에 에셋 라이브러리가 포함되어 있습니다','所选文件夹包含资产库','所選資料夾包含資產庫'],
 local_source_changed:['Source files changed; choose them again','元のファイルが変更されました。選択し直してください','원본 파일이 변경되었습니다. 다시 선택하세요','源文件已更改，请重新选择','來源檔案已變更，請重新選擇'],
 local_import_too_many_files:['Too many files in this import','インポートするファイルが多すぎます','가져올 파일이 너무 많습니다','本次导入的文件过多','此次匯入的檔案過多'],
 local_draft_changed:['This import is already queued; start a new import for changes','このインポートは待機列にあります。変更する場合は新しく開始してください','이미 대기열에 있는 가져오기입니다. 변경하려면 새로 시작하세요','此次导入已排队，更改内容请新建导入','此次匯入已排入佇列，變更內容請另建匯入'],
};
export function localImportText(locale:Locale,key:string):string {
 const index=({en:0,ja:1,ko:2,'zh-Hans':3,'zh-Hant':4} as Record<string,number>)[locale]??0;
 return words[key]?.[index]??nativeWorkflowText(locale,key);
}
