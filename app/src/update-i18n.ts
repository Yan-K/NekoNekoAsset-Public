import type {Locale} from './model';
const labels:Record<string,[string,string,string,string,string]>={
 title:['App updates','アプリの更新','앱 업데이트','应用更新','App 更新'],
 check:['Check for updates','更新を確認','업데이트 확인','检查更新','檢查更新'],
 checking:['Checking for updates…','更新を確認中…','업데이트 확인 중…','正在检查更新…','正在檢查更新…'],
 current:['Current version','現在のバージョン','현재 버전','当前版本','目前版本'],
 latest:['You’re up to date','最新バージョンです','최신 버전입니다','已是最新版本','已是最新版本'],
 available:['Update available · v{version}','更新があります · v{version}','업데이트 사용 가능 · v{version}','有可用更新 · v{version}','有可用更新 · v{version}'],
 install:['Update and restart','更新して再起動','업데이트 후 다시 시작','更新并重启','更新並重新啟動'],
 later:['Later','後で','나중에','稍后','稍後'],
 close:['Close','閉じる','닫기','关闭','關閉'],
 release:['Release notes','更新内容','업데이트 내용','更新说明','更新說明'],
 downloading:['Downloading update…','更新をダウンロード中…','업데이트 다운로드 중…','正在下载更新…','正在下載更新…'],
 verifying:['Verifying update…','更新を検証中…','업데이트 확인 중…','正在验证更新…','正在驗證更新…'],
 installing:['Installing update…','更新をインストール中…','업데이트 설치 중…','正在安装更新…','正在安裝更新…'],
 syncResumes:['Sync will continue after the restart.','同期は再起動後に続きから再開します。','동기화는 다시 시작한 후 이어서 진행됩니다.','同步将在重启后从中断处继续。','同步會在重新啟動後從中斷處繼續。'],
 bridgeDownload:['Download Bridge .unitypackage','Bridge .unitypackage をダウンロード','Bridge .unitypackage 다운로드','下载 Bridge .unitypackage','下載 Bridge .unitypackage'],
 bridgeUpdate:['Bridge update · v{version}','Bridge 更新 · v{version}','Bridge 업데이트 · v{version}','Bridge 更新 · v{version}','Bridge 更新 · v{version}'],
 bridgeUnknown:['Bridge version unknown','Bridge バージョン不明','Bridge 버전 알 수 없음','Bridge 版本未知','Bridge 版本未知'],
 bridgeFailed:['Could not check Bridge updates','Bridge の更新を確認できませんでした','Bridge 업데이트를 확인하지 못했습니다','无法检查 Bridge 更新','無法檢查 Bridge 更新'],
 retry:['Retry','再試行','다시 시도','重试','重試'],
 updates_unavailable:['Could not check updates. Try again.','更新を確認できませんでした。再試行してください。','업데이트를 확인하지 못했습니다. 다시 시도하세요.','无法检查更新，请重试。','無法檢查更新，請重試。'],
 updates_invalid_manifest:['Update information is invalid. Try again later.','更新情報が無効です。後で再試行してください。','업데이트 정보가 잘못되었습니다. 나중에 다시 시도하세요.','更新信息无效，请稍后重试。','更新資訊無效，請稍後重試。'],
 updates_busy:['Other work is still finishing. Try again shortly.','ほかの処理がまだ終わっていません。少し待ってから再試行してください。','다른 작업이 아직 끝나지 않았습니다. 잠시 후 다시 시도하세요.','其他工作尚未结束，请稍后重试。','其他工作尚未結束，請稍後重試。'],
 updates_busy_library:['The library is still opening.','ライブラリを開いています。','라이브러리를 여는 중입니다.','资产库仍在打开中。','資產庫仍在開啟中。'],
 updates_busy_download:['Downloads are in progress. Update after they finish or are cancelled.','ダウンロード中です。完了またはキャンセル後に更新できます。','다운로드 중입니다. 완료하거나 취소한 후 업데이트할 수 있습니다.','下载进行中，完成或取消后即可更新。','下載進行中，完成或取消後即可更新。'],
 updates_busy_import:['Files are being imported. Update after the import finishes.','ファイルを追加しています。完了後に更新できます。','파일을 가져오는 중입니다. 완료 후 업데이트할 수 있습니다.','正在导入文件，完成后即可更新。','正在匯入檔案，完成後即可更新。'],
 updates_busy_move:['The library is being moved. Update after the move finishes.','ライブラリを移動しています。完了後に更新できます。','라이브러리를 이동하는 중입니다. 완료 후 업데이트할 수 있습니다.','正在迁移资产库，完成后即可更新。','正在搬移資產庫，完成後即可更新。'],
 updates_busy_backup:['A backup is being created. Update after it finishes.','バックアップを作成しています。完了後に更新できます。','백업을 만드는 중입니다. 완료 후 업데이트할 수 있습니다.','正在创建备份，完成后即可更新。','正在建立備份，完成後即可更新。'],
 updates_busy_restore:['A backup is being restored. Update after it finishes.','バックアップを復元しています。完了後に更新できます。','백업을 복원하는 중입니다. 완료 후 업데이트할 수 있습니다.','正在恢复备份，完成后即可更新。','正在還原備份，完成後即可更新。'],
 updates_busy_export:['A ZIP is being exported. Update after it finishes.','ZIP を書き出しています。完了後に更新できます。','ZIP을 내보내는 중입니다. 완료 후 업데이트할 수 있습니다.','正在导出 ZIP，完成后即可更新。','正在匯出 ZIP，完成後即可更新。'],
 updates_busy_delete:['Assets are being deleted. Update after it finishes.','アセットを削除しています。完了後に更新できます。','에셋을 삭제하는 중입니다. 완료 후 업데이트할 수 있습니다.','正在删除资产，完成后即可更新。','正在刪除資產，完成後即可更新。'],
 updates_busy_unity:['Files are being imported into Unity. Update after it finishes.','Unity にインポートしています。完了後に更新できます。','Unity로 가져오는 중입니다. 완료 후 업데이트할 수 있습니다.','正在导入 Unity，完成后即可更新。','正在匯入 Unity，完成後即可更新。'],
 updates_busy_ai:['AI tagging is in progress. Update after it finishes.','AIタグ付けを実行しています。完了後に更新できます。','AI 태그 작업이 진행 중입니다. 완료 후 업데이트할 수 있습니다.','AI 标签处理中，完成后即可更新。','AI 標籤處理中，完成後即可更新。'],
 updates_busy_login:['A store sign-in is in progress. Update after it finishes.','ストアにログインしています。完了後に更新できます。','스토어 로그인이 진행 중입니다. 완료 후 업데이트할 수 있습니다.','正在登录商店，完成后即可更新。','正在登入商店，完成後即可更新。'],
 updates_busy_job:['A task in Activity is still running. Update after it finishes.','アクティビティのタスクが実行中です。完了後に更新できます。','활동의 작업이 아직 실행 중입니다. 완료 후 업데이트할 수 있습니다.','活动中仍有任务在运行，完成后即可更新。','活動中仍有工作在執行，完成後即可更新。'],
 updates_busy_window:['Another window is open. Close it to update.','別の画面が開いています。閉じてから更新してください。','다른 창이 열려 있습니다. 닫은 후 업데이트하세요.','另一个窗口已打开，请先关闭再更新。','另一個視窗已開啟，請先關閉再更新。'],
 updates_busy_settings:['Settings is open. Close it to update.','設定を開いています。閉じてから更新してください。','설정이 열려 있습니다. 닫은 후 업데이트하세요.','设置页面已打开，请先关闭再更新。','設定頁面已開啟，請先關閉再更新。'],
 updates_busy_ai_chat:['The AI chat has unsent input or a reply in progress.','AIチャットに未送信の入力または応答待ちがあります。','AI 채팅에 보내지 않은 입력이나 진행 중인 응답이 있습니다.','AI 对话有未发送的内容或正在等待回复。','AI 對話有未送出的內容或正在等待回覆。'],
 updates_busy_saving:['Saving changes…','変更を保存しています…','변경 사항을 저장하는 중…','正在保存更改…','正在儲存變更…'],
 updates_portable_unsupported:['Updates require the installed Windows app.','更新には Windows インストール版が必要です。','업데이트하려면 설치된 Windows 앱이 필요합니다.','更新需要 Windows 安装版应用。','更新需要 Windows 安裝版 App。'],
 updates_stale:['Check for updates again.','更新をもう一度確認してください。','업데이트를 다시 확인하세요.','请重新检查更新。','請重新檢查更新。'],
 updates_download_failed:['Update download failed. Try again.','更新のダウンロードに失敗しました。再試行してください。','업데이트 다운로드에 실패했습니다. 다시 시도하세요.','更新下载失败，请重试。','更新下載失敗，請重試。'],
 updates_signature_invalid:['Update verification failed. Nothing was installed.','更新の検証に失敗しました。インストールされていません。','업데이트 검증에 실패했습니다. 설치되지 않았습니다.','更新验证失败，未安装任何更新。','更新驗證失敗，尚未安裝更新。'],
 updates_install_failed:['Could not start the installer. Try again.','インストーラーを開始できませんでした。再試行してください。','설치 프로그램을 시작하지 못했습니다. 다시 시도하세요.','无法启动安装程序，请重试。','無法啟動安裝程式，請重試。'],
 operationFailed:['Operation failed. Try again.','操作に失敗しました。再試行してください。','작업에 실패했습니다. 다시 시도하세요.','操作失败，请重试。','操作失敗，請重試。'],
};
const locales:Locale[]=['en','ja','ko','zh-Hans','zh-Hant'];
export function updateText(locale:Locale,key:string):string{return labels[key]?.[locales.indexOf(locale)]??labels.operationFailed[locales.indexOf(locale)];}

// Mirrors native updates::blocking_job: store syncs resume from their checkpoints after the restart.
const jobBlockers:Record<string,string>={download:'updates_busy_download','storage:import_local':'updates_busy_import','storage:migrate_root':'updates_busy_move','storage:export_backup':'updates_busy_backup','storage:restore_backup':'updates_busy_restore','storage:export_zip':'updates_busy_export','storage:delete_local_products':'updates_busy_delete','unity:import':'updates_busy_unity',boothLogin:'updates_busy_login',gumroadLogin:'updates_busy_login',jinxxyLogin:'updates_busy_login'};
const active=(job:{state:string})=>job.state==='queued'||job.state==='running';
export const isResumableSync=(job:{type:string;state:string})=>active(job)&&/^(booth|gumroad|jinxxy)(Sync|Update)$/.test(job.type);
export function updateJobBlocker(jobs:{type:string;state:string}[]):string{
 const job=jobs.find(job=>active(job)&&!isResumableSync(job));
 return !job?'':jobBlockers[job.type]??(job.type.startsWith('ai:')?'updates_busy_ai':'updates_busy_job');
}

// Bridge releases share numeric App versions; absent or unrecognized versions stay unknown.
export function isNewerBridge(latest:string,current:string|null|undefined):boolean{
 const parse=(value:string)=>/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(value)?.slice(1).map(Number);
 const a=parse(latest),b=current?parse(current):undefined;
 if(!a||!b)return false;
 for(let i=0;i<3;i++){if(a[i]!==b[i])return a[i]>b[i];}
 return false;
}
