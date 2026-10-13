import type {Locale} from './model';
const labels:Record<string,string[]>={
 'catalogue.hide_store_only':['Only store products can be hidden','非表示にできるのはストア商品のみです','스토어 상품만 숨길 수 있습니다','仅可隐藏商店商品','僅可隱藏商店產品'],
 local_product_required:['Only offline products can be deleted here','削除できるのはオフライン商品のみです','오프라인 상품만 삭제할 수 있습니다','此处仅可删除离线商品','此處僅可刪除離線產品'],
 local_delete_unsafe_path:['Product folder could not be safely verified; nothing deleted','商品フォルダーの安全性を確認できません。削除していません','상품 폴더를 안전하게 확인할 수 없어 삭제하지 않았습니다','无法安全验证商品文件夹，未删除任何内容','無法安全確認產品資料夾，未刪除任何內容'],
 local_delete_shared_path:['Product folder is shared with another product; nothing deleted','商品フォルダーは別の商品と共有されています。削除していません','다른 상품과 공유된 폴더여서 삭제하지 않았습니다','商品文件夹被其他商品共用，未删除任何内容','產品資料夾由其他產品共用，未刪除任何內容'],
 local_delete_cleanup_failed:['Product entries deleted; folder cleanup failed. Retry in Activity.','商品は削除済みですが、フォルダーの削除に失敗しました。履歴で再試行してください。','상품 항목은 삭제됐지만 폴더 정리에 실패했습니다. 활동에서 다시 시도하세요.','商品记录已删除，但文件夹清理失败。请在活动页重试。','產品紀錄已刪除，但資料夾清理失敗。請到活動頁重試。'],
 local_delete_rollback_failed:['Deletion stopped; restoring product folders failed. Retry in Activity.','削除は停止しましたが、フォルダーの復元に失敗しました。履歴で再試行してください。','삭제를 중단했지만 상품 폴더 복원에 실패했습니다. 활동에서 다시 시도하세요.','删除已停止，但商品文件夹还原失败。请在活动页重试。','刪除已停止，但產品資料夾還原失敗。請到活動頁重試。'],
 local_delete_retry_required:['Deletion stopped; product folders restored. Retry in Activity.','削除は停止し、フォルダーを復元しました。履歴で再試行してください。','삭제를 중단하고 상품 폴더를 복원했습니다. 활동에서 다시 시도하세요.','删除已停止，商品文件夹已还原。请在活动页重试。','刪除已停止，產品資料夾已還原。請到活動頁重試。'],
 link_expired:['Download link expired; retry to get a new link','ダウンロードリンクの期限が切れました。再試行で再取得します','다운로드 링크 만료 · 다시 시도하면 새 링크를 가져옵니다','下载链接已过期，重试以取得新链接','下載連結已過期，重試以取得新連結'],
 manifest_incomplete:['Product file list is incomplete; finish syncing and retry','商品のファイル一覧が未完了です。同期完了後に再試行してください','상품 파일 목록 미완료 · 동기화 완료 후 다시 시도하세요','商品文件列表尚未完整，请完成同步后重试','商品檔案清單尚未完整，請完成同步後重試'],
 network_failed:['Download connection failed; retry','ダウンロード接続に失敗しました。再試行してください','다운로드 연결 실패 · 다시 시도하세요','下载连接失败，请重试','下載連線失敗，請重試'],
 network_timeout:['Download connection timed out; retry','ダウンロード接続がタイムアウトしました。再試行してください','다운로드 연결 시간 초과 · 다시 시도하세요','下载连接超时，请重试','下載連線逾時，請重試'],
 rate_limited:['Store rate limit reached; retry later','ストアのリクエスト制限に達しました。後で再試行してください','스토어 요청 제한 · 나중에 다시 시도하세요','商店请求达到频率限制，请稍后重试','商店請求達到頻率限制，請稍後重試'],
 source_changed:['File information changed during download; update and retry','ダウンロード中にファイル情報が変更されました。更新して再試行してください','다운로드 중 파일 정보 변경 · 업데이트 후 다시 시도하세요','下载期间文件信息已更改，请更新后重试','下載期間檔案資訊已變更，請更新後重試'],
 download_size_mismatch:['Downloaded size does not match the file; retry','ダウンロードしたサイズが一致しません。再試行してください','다운로드 파일 크기 불일치 · 다시 시도하세요','下载文件大小不符，请重试','下載檔案大小不符，請重試'],
 download_resolver_failed:['Store could not provide a download link; retry','ストアからダウンロードリンクを取得できません。再試行してください','스토어 다운로드 링크를 가져오지 못했습니다 · 다시 시도하세요','无法从商店取得下载链接，请重试','無法從商店取得下載連結，請重試'],
 download_http_failed:['Download server returned an error; retry later','ダウンロードサーバーでエラーが発生しました。後で再試行してください','다운로드 서버 오류 · 나중에 다시 시도하세요','下载服务器发生错误，请稍后重试','下載伺服器發生錯誤，請稍後重試'],
 manifest_changed:['Product file list changed; start the download again','商品のファイル一覧が変更されました。ダウンロードをやり直してください','상품 파일 목록 변경 · 다운로드를 다시 시작하세요','商品文件列表已更改，请重新开始下载','商品檔案清單已變更，請重新開始下載'],
 local_membership_changed:['This product’s files changed; start the file action again','商品のファイルが変更されました。操作をやり直してください','이 상품의 파일이 변경되었습니다. 파일 작업을 다시 시작하세요','此商品的文件已更改，请重新开始文件操作','此商品的檔案已變更，請重新開始檔案操作'],
 storage_busy:['A file action is already in progress','ファイル操作が進行中です','파일 작업이 진행 중입니다','文件操作正在进行中','檔案操作正在進行中'],
 library_not_configured:['Set a library location before continuing','続行する前に保存先を設定してください','계속하려면 라이브러리 위치를 설정하세요','继续前请设置资产库位置','繼續前請設定資產庫位置'],
 nativeUnavailable:['Open the desktop app to use this action','この操作はデスクトップアプリで実行してください','이 작업은 데스크톱 앱에서 실행하세요','请在桌面应用中执行此操作','請在桌面程式中執行此操作'],
 operationFailed:['Action failed','操作に失敗しました','작업 실패','操作失败','操作失敗'],
 auth:['Sign-in required','ログインが必要です','로그인 필요','需要登录','需要登入'],
 offline:['Library location unavailable','ライブラリの場所にアクセスできません','라이브러리 위치를 사용할 수 없음','资产库位置不可用','資產庫位置無法使用'],
 missing:['Required files are missing','必要なファイルがありません','필요한 파일이 없음','缺少所需文件','缺少必要檔案'],
 archive:['Archive could not be extracted; original retained','展開できませんでした。元ファイルは保持されています','압축을 풀 수 없음 · 원본 보존','无法解压，原文件已保留','無法解壓，原檔已保留'],
 space:['Not enough free space','空き容量が不足しています','저장 공간 부족','可用空间不足','可用空間不足'],
 conflict:['Edit changed on another device','別の端末で編集されています','다른 기기에서 수정됨','其他设备已修改','其他裝置已修改'],
 permission:['Access denied','アクセスが拒否されました','액세스 거부','访问被拒绝','存取遭拒'],
 validation:['Check the selected file or location','選択したファイルまたは場所を確認してください','선택한 파일 또는 위치 확인','请检查所选文件或位置','請檢查選取的檔案或位置'],
 rate:['Source is busy; retry later','取得元が混雑しています。後で再試行してください','소스 사용 중 · 나중에 재시도','来源繁忙，请稍后重试','來源忙碌，請稍後重試'],
};
export function nativeErrorText(locale:Locale,code:string):string|undefined {
 let key=labels[code]?code:'';const lower=code.toLowerCase();
 const nativeCode=lower.split('.').at(-1)??lower;
 if(!key&&['store_identity_changed','store_identity_mismatch','account_identity_mismatch'].includes(nativeCode))key='auth';
 if(!key&&/^(jinxxy|gumroad)_(source_unsupported|scope_changed|pagination_unproven)$/.test(nativeCode))key='operationFailed';
 if(!key&&/^(file_unavailable|downloads?_unavailable)$/.test(nativeCode))key='missing';
 if(!key&&['collector_unsupported','unsupported_image'].includes(nativeCode))key='validation';
 if(!key&&['unsafe_download_url','unsafe_redirect','redirect_limit'].includes(nativeCode))key='operationFailed';
 if(!key&&/auth|login|session|expired/.test(lower))key='auth';
 if(!key&&/offline|root_unavailable|root_offline/.test(lower))key='offline';
 if(!key&&/low_space|disk_full|insufficient_space/.test(lower))key='space';
 if(!key&&/file_missing|file_not_usable|original_unavailable|manifest_incomplete|integrity/.test(lower))key='missing';
 if(!key&&/archive|extract|password|volume/.test(lower))key='archive';
 if(!key&&/conflict|undo/.test(lower))key='conflict';
 if(!key&&/permission|access_denied/.test(lower))key='permission';
 if(!key&&/invalid_|unsafe|newer_schema|version_unsupported/.test(lower))key='validation';
 if(!key&&/rate_limit/.test(lower))key='rate';
 if(!key&&(/^(catalogue|storage|booth|media|native|file)\./.test(lower)||code===code.toUpperCase()&&/[A-Z_]+/.test(code)||/_/.test(code)))key='operationFailed';
 return labels[key]?.[['en','ja','ko','zh-Hans','zh-Hant'].indexOf(locale)];
}
