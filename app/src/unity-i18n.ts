import type {Locale} from './model';
import {nativeErrorText} from './error-i18n';
import type {UnityPreparation} from './backend';
const locales:Locale[]=['en','ja','ko','zh-Hans','zh-Hant'];
const text:Record<string,string[]>={
 selectAll:['Select all','すべて選択','모두 선택','全选','全選'],
 project:['Project','プロジェクト','프로젝트','项目','專案'],
 files:['Files','ファイル','파일','文件','檔案'],
 packages:['.unitypackage','.unitypackage','.unitypackage','.unitypackage','.unitypackage'],
 singleFiles:['Individual files','個別ファイル','개별 파일','单个文件','單一檔案'],
 refresh:['Refresh','更新','새로 고침','刷新','重新整理'],
 send:['Send to Unity','Unity に送信','Unity로 보내기','发送到 Unity','傳送到 Unity'],
 close:['Close','閉じる','닫기','关闭','關閉'],
 cancel:['Cancel','キャンセル','취소','取消','取消'],
 retry:['Retry selection','再選択','다시 선택','重新选择','重新選取'],
 results:['Results','結果','결과','结果','結果'],
 query:['Check status','状態を確認','상태 확인','查询状态','查詢狀態'],
 selected:['Selected','選択済み','선택됨','已选择','已選取'],
 original:['Original','元ファイル','원본','原始文件','原始檔案'],
 extracted:['Extracted','展開済み','압축 해제됨','已解压','已解壓'],
 busy:['Busy','処理中','사용 중','忙碌','忙碌'],
 noEditors:['No running Bridge Editors','実行中の Bridge Editor はありません','실행 중인 Bridge Editor 없음','没有运行中的 Bridge Editor','沒有執行中的 Bridge Editor'],
 noFiles:['No available files','利用できるファイルはありません','사용 가능한 파일 없음','没有可用文件','沒有可用檔案'],
 noResults:['No file results yet','ファイル結果はまだありません','아직 파일 결과가 없음','尚无文件结果','尚無檔案結果'],
 preparing:['Preparing files','ファイルを準備中','파일 준비 중','准备文件','準備檔案'],
 waitingForEditor:['Waiting for Unity to become available','Unity が操作可能になるまで待機中','Unity를 사용할 수 있을 때까지 대기 중','等待 Unity 就绪','等待 Unity 就緒'],
 verify:['Verifying files in Unity','Unity でファイルを検証中','Unity에서 파일 확인 중','Unity 正在验证文件','Unity 正在驗證檔案'],
 readPackage:['Reading package in Unity','Unity でパッケージを読み込み中','Unity에서 패키지 읽는 중','Unity 正在读取包','Unity 正在讀取套件'],
 checkTargets:['Checking Unity targets','Unity の保存先を確認中','Unity 대상 확인 중','正在检查 Unity 目标','正在檢查 Unity 目標'],
 staging:['Sending files','ファイルを送信中','파일 전송 중','传送文件','傳送檔案'],
 waitingForUser:['Waiting for Unity confirmation','Unity での確認待ち','Unity 확인 대기 중','等待 Unity 确认','等待 Unity 確認'],
 importing:['Importing','インポート中','가져오는 중','正在导入','正在匯入'],
 refreshing:['Refreshing assets','アセット更新中','에셋 새로 고침 중','刷新资源','重新整理資產'],
 completed:['Completed','完了','완료','已完成','已完成'],
 cancelled:['Cancelled','キャンセル済み','취소됨','已取消','已取消'],
 failed:['Failed','失敗','실패','失败','失敗'],
 interrupted:['Interrupted','中断','중단됨','已中断','已中斷'],
 unresolved:['Result unconfirmed','結果未確認','결과 미확인','结果尚未确认','結果尚未確認'],
 applied:['Applied','適用済み','적용됨','已应用','已套用'],
 skipped:['Skipped','スキップ','건너뜀','已跳过','已略過'],
 UNITY_UNAVAILABLE:['Unity is unavailable. The existing request will be checked again.','Unity に接続できません。同じリクエストを再確認します。','Unity에 연결할 수 없습니다. 기존 요청을 다시 확인합니다.','暂时无法连接 Unity，将再次查询原请求。','暫時無法連線 Unity，將再次查詢原請求。'],
 UNITY_TIMEOUT:['Unity did not respond. Check the existing request.','Unity の応答がありません。同じリクエストを確認してください。','Unity 응답이 없습니다. 기존 요청을 확인하세요.','Unity 未响应，请查询原请求。','Unity 未回應，請查詢原請求。'],
 UNITY_BUSY:['Unity is busy','Unity は処理中です','Unity가 사용 중입니다','Unity 正在忙碌','Unity 正在忙碌'],
 UNITY_DISABLED:['The Bridge was disabled','Bridge が無効になりました','Bridge가 비활성화되었습니다','Bridge 已停用','Bridge 已停用'],
 UNITY_OTHER_DEVICE:['Check this import’s result on the device that sent it.','送信した端末で、このインポートの結果を確認してください。','이 가져오기를 보낸 기기에서 결과를 확인하세요.','请在发送此导入的设备上查看结果。','請在傳送此匯入的裝置上查看結果。'],
 UNITY_SELECTION_EMPTY:['Select files first','ファイルを選択してください','파일을 선택하세요','请先选择文件','請先選取檔案'],
 UNITY_SOURCE_CHANGED:['Files changed. Refresh your selection.','ファイルが変更されました。選択を更新してください。','파일이 변경되었습니다. 선택을 새로 고치세요.','文件已改变，请刷新选取。','檔案已變更，請重新整理選取。'],
 UNITY_RESULT_UNRESOLVED:['The result is unconfirmed; the request cannot be replayed.','結果が未確認のため再送できません。','결과가 미확인 상태여서 요청을 다시 보낼 수 없습니다.','结果尚未确认，无法重送请求。','結果尚未確認，無法重送請求。'],
 UNITY_RETRY_UNAVAILABLE:['No safely retryable files. Review the results.','安全に再送できるファイルがありません。結果を確認してください。','안전하게 재시도할 파일이 없습니다. 결과를 확인하세요.','没有可安全重试的文件，请查看结果。','沒有可安全重試的檔案，請查看結果。'],
 UNITY_INTERRUPTED:['Import interrupted; applied files are retained','インポート中断。適用済みファイルは保持されます','가져오기가 중단되었습니다. 적용된 파일은 유지됩니다','导入已中断，已应用的文件保留','匯入已中斷，已套用的檔案保留'],
 UNITY_SELECTION_TOO_LARGE:['Too many file descriptors; select fewer files','ファイル情報が多すぎます。選択を減らしてください','파일 정보가 너무 많습니다. 선택을 줄이세요','文件信息过多，请减少选取','檔案資訊過多，請減少選取'],
 UNITY_INVALID_RESPONSE:['Unity returned an invalid response','Unity の応答が無効です','Unity 응답이 올바르지 않습니다','Unity 回应无效','Unity 回應無效'],
 unity_partial_failure:['Some files could not be imported','一部ファイルをインポートできませんでした','일부 파일을 가져올 수 없습니다','部分文件导入失败','部分檔案匯入失敗'],
 unity_import_failed:['Import failed; review file results','インポート失敗。ファイル結果を確認してください','가져오기에 실패했습니다. 파일 결과를 확인하세요','导入失败，请查看文件结果','匯入失敗，請查看檔案結果'],
 unity_interrupted:['Import interrupted','インポートが中断されました','가져오기가 중단되었습니다','导入已中断','匯入已中斷'],
 unity_path_collision:['Files share a destination path. Select one.','保存先が同じファイルがあります。1つ選択してください。','대상 경로가 같은 파일입니다. 하나를 선택하세요.','文件的目标路径相同，请选取其中一个。','檔案的目的路徑相同，請選取其中一個。'],
};
export function unityText(locale:Locale,key:string):string{return text[key==='compiling'?'refreshing':key]?.[locales.indexOf(locale)]??key;}
export function unityProgressLabel(locale:Locale,value:UnityPreparation&{stage?:string}):string{
 if(value.stage==='preparing'&&value.preparationWaitingForEditor)return unityText(locale,'waitingForEditor');
 const phase=value.stage==='preparing'?value.preparationPhase:null;
 const label=unityText(locale,phase||value.stage||'preparing');
 if(!phase)return label;
 if(phase==='checkTargets')return typeof value.preparationTotalEntries==='number'&&value.preparationTotalEntries>0?`${label} · ${(value.preparationEntries??0).toLocaleString(locale)}/${value.preparationTotalEntries.toLocaleString(locale)}`:label;
 const bytes=(size:number)=>size>=1_073_741_824?`${(size/1_073_741_824).toFixed(2)} GiB`:`${(size/1_048_576).toFixed(1)} MiB`;
 return typeof value.preparationTotalBytes==='number'&&value.preparationTotalBytes>0?`${label} · ${bytes(value.preparationBytes??0)}/${bytes(value.preparationTotalBytes)}`:label;
}
export function unityError(locale:Locale,key:string):string{
 const aliases:Record<string,string>={unity_leaf_stale:'UNITY_SOURCE_CHANGED',unity_leaf_invalid:'UNITY_SOURCE_CHANGED',unity_selection_empty:'UNITY_SELECTION_EMPTY',unity_payload_too_large:'UNITY_SELECTION_TOO_LARGE'};
 return text[aliases[key]??key]?.[locales.indexOf(locale)]??nativeErrorText(locale,key)??nativeErrorText(locale,'operationFailed')??'Operation failed';
}
