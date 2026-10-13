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
 busy:['Finish current work before updating','現在の操作を終えてから更新してください','현재 작업을 마친 후 업데이트하세요','请完成当前操作后再更新','請完成目前操作後再更新'],
 bridgeDownload:['Download Bridge .unitypackage','Bridge .unitypackage をダウンロード','Bridge .unitypackage 다운로드','下载 Bridge .unitypackage','下載 Bridge .unitypackage'],
 bridgeUpdate:['Bridge update · v{version}','Bridge 更新 · v{version}','Bridge 업데이트 · v{version}','Bridge 更新 · v{version}','Bridge 更新 · v{version}'],
 bridgeUnknown:['Bridge version unknown','Bridge バージョン不明','Bridge 버전 알 수 없음','Bridge 版本未知','Bridge 版本未知'],
 bridgeFailed:['Could not check Bridge updates','Bridge の更新を確認できませんでした','Bridge 업데이트를 확인하지 못했습니다','无法检查 Bridge 更新','無法檢查 Bridge 更新'],
 retry:['Retry','再試行','다시 시도','重试','重試'],
 updates_unavailable:['Could not check updates. Try again.','更新を確認できませんでした。再試行してください。','업데이트를 확인하지 못했습니다. 다시 시도하세요.','无法检查更新，请重试。','無法檢查更新，請重試。'],
 updates_invalid_manifest:['Update information is invalid. Try again later.','更新情報が無効です。後で再試行してください。','업데이트 정보가 잘못되었습니다. 나중에 다시 시도하세요.','更新信息无效，请稍后重试。','更新資訊無效，請稍後重試。'],
 updates_busy:['Finish current work, then try again.','現在の操作を終えてから再試行してください。','현재 작업을 마친 후 다시 시도하세요.','请完成当前操作后重试。','請完成目前操作後重試。'],
 updates_portable_unsupported:['Updates require the installed Windows app.','更新には Windows インストール版が必要です。','업데이트하려면 설치된 Windows 앱이 필요합니다.','更新需要 Windows 安装版应用。','更新需要 Windows 安裝版 App。'],
 updates_stale:['Check for updates again.','更新をもう一度確認してください。','업데이트를 다시 확인하세요.','请重新检查更新。','請重新檢查更新。'],
 updates_download_failed:['Update download failed. Try again.','更新のダウンロードに失敗しました。再試行してください。','업데이트 다운로드에 실패했습니다. 다시 시도하세요.','更新下载失败，请重试。','更新下載失敗，請重試。'],
 updates_signature_invalid:['Update verification failed. Nothing was installed.','更新の検証に失敗しました。インストールされていません。','업데이트 검증에 실패했습니다. 설치되지 않았습니다.','更新验证失败，未安装任何更新。','更新驗證失敗，尚未安裝更新。'],
 updates_install_failed:['Could not start the installer. Try again.','インストーラーを開始できませんでした。再試行してください。','설치 프로그램을 시작하지 못했습니다. 다시 시도하세요.','无法启动安装程序，请重试。','無法啟動安裝程式，請重試。'],
 operationFailed:['Operation failed. Try again.','操作に失敗しました。再試行してください。','작업에 실패했습니다. 다시 시도하세요.','操作失败，请重试。','操作失敗，請重試。'],
};
const locales:Locale[]=['en','ja','ko','zh-Hans','zh-Hant'];
export function updateText(locale:Locale,key:string):string{return labels[key]?.[locales.indexOf(locale)]??labels.operationFailed[locales.indexOf(locale)];}

// Bridge releases share numeric App versions; absent or unrecognized versions stay unknown.
export function isNewerBridge(latest:string,current:string|null|undefined):boolean{
 const parse=(value:string)=>/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(value)?.slice(1).map(Number);
 const a=parse(latest),b=current?parse(current):undefined;
 if(!a||!b)return false;
 for(let i=0;i<3;i++){if(a[i]!==b[i])return a[i]>b[i];}
 return false;
}
