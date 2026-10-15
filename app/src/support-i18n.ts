import type {Locale} from './model';

type SupportLabel='copyDiagnostics'|'copying'|'copied'|'copyFailed';
const labels:Record<Locale,Record<SupportLabel,string>>={
 en:{copyDiagnostics:'Copy diagnostics',copying:'Copying…',copied:'Copied',copyFailed:'Could not copy. Try again.'},
 ja:{copyDiagnostics:'診断情報をコピー',copying:'コピー中…',copied:'コピーしました',copyFailed:'コピーできませんでした。再試行してください。'},
 ko:{copyDiagnostics:'진단 정보 복사',copying:'복사 중…',copied:'복사됨',copyFailed:'복사하지 못했습니다. 다시 시도하세요.'},
 'zh-Hans':{copyDiagnostics:'复制诊断信息',copying:'正在复制…',copied:'已复制',copyFailed:'无法复制，请重试。'},
 'zh-Hant':{copyDiagnostics:'複製診斷資訊',copying:'複製中…',copied:'已複製',copyFailed:'無法複製，請重試。'},
};
export function supportText(locale:Locale,key:SupportLabel):string{return labels[locale][key];}
