import type {Locale} from './model';
const labels:Record<string,string[]>={
 SYNC_INCOMPLETE:['Some source information is still unconfirmed; retry sync','取得元の情報を確認しきれていません。同期を再試行してください','일부 소스 정보를 확인하지 못했습니다. 동기화를 다시 시도하세요','部分来源信息尚未确认，请重试同步','部分來源資訊尚未確認，請重試同步'],
 NO_RECOGNIZED_ROWS:['Could not read the store list; retry or copy diagnostics','ストアの一覧を読み取れません。再試行するか診断情報をコピーしてください','스토어 목록을 읽을 수 없습니다. 다시 시도하거나 진단 정보를 복사하세요','无法读取商店列表，请重试或复制诊断信息','無法讀取商店清單，請重試或複製診斷資訊'],
 SOURCE_MARKER_UNKNOWN:['The store list format was not recognized; copy diagnostics if retry fails','ストアの一覧形式を認識できません。再試行しても失敗する場合は診断情報をコピーしてください','스토어 목록 형식을 인식하지 못했습니다. 재시도 후에도 실패하면 진단 정보를 복사하세요','无法识别商店列表格式，重试仍失败时请复制诊断信息','無法辨識商店清單格式，重試仍失敗時請複製診斷資訊'],
 SOURCE_HTTP_FAILED:['The store request failed; retry later','ストアへのリクエストに失敗しました。後で再試行してください','스토어 요청에 실패했습니다. 나중에 다시 시도하세요','商店请求失败，请稍后重试','商店請求失敗，請稍後重試'],
 RESPONSE_ROUTE_CHANGED:['The store opened an unexpected page; retry or sign in again','ストアが想定外のページを開きました。再試行するか再ログインしてください','스토어가 예상하지 못한 페이지를 열었습니다. 다시 시도하거나 다시 로그인하세요','商店打开了非预期页面，请重试或重新登录','商店開啟了非預期頁面，請重試或重新登入'],
 BOOTH_OPERATION_FAILED:['BOOTH action failed; retry or copy diagnostics','BOOTHの操作に失敗しました。再試行するか診断情報をコピーしてください','BOOTH 작업에 실패했습니다. 다시 시도하거나 진단 정보를 복사하세요','BOOTH 操作失败，请重试或复制诊断信息','BOOTH 操作失敗，請重試或複製診斷資訊'],
 NETWORK_TIMEOUT:['Connection timed out; retry','接続がタイムアウトしました。再試行してください','연결 시간이 초과되었습니다. 다시 시도하세요','连接超时，请重试','連線逾時，請重試'],
 NETWORK_UNAVAILABLE:['Connection failed; check the network and retry','接続できません。ネットワークを確認して再試行してください','연결에 실패했습니다. 네트워크를 확인하고 다시 시도하세요','连接失败，请检查网络后重试','連線失敗，請檢查網路後重試'],
 CHECKPOINT_INVALID:['Saved sync progress could not be resumed; copy diagnostics if retry fails','保存した同期処理を再開できません。再試行しても失敗する場合は診断情報をコピーしてください','저장된 동기화를 재개할 수 없습니다. 재시도 후에도 실패하면 진단 정보를 복사하세요','无法恢复已保存的同步进度，重试仍失败时请复制诊断信息','無法恢復已儲存的同步進度，重試仍失敗時請複製診斷資訊'],
 AUTH_CHALLENGE:['The store requires a sign-in check; sign in again','ストアでログインの確認が必要です。再ログインしてください','스토어에서 로그인 확인이 필요합니다. 다시 로그인하세요','商店需要验证登录，请重新登录','商店需要驗證登入，請重新登入'],
 AUTH_TIMEOUT:['Sign-in timed out; sign in again','ログインがタイムアウトしました。再ログインしてください','로그인 시간이 초과되었습니다. 다시 로그인하세요','登录超时，请重新登录','登入逾時，請重新登入'],
 COLLECTOR_UNSUPPORTED:['The store page format is unsupported; copy diagnostics','ストアのページ形式に対応できません。診断情報をコピーしてください','지원하지 않는 스토어 페이지 형식입니다. 진단 정보를 복사하세요','暂不支持此商店页面格式，请复制诊断信息','暫不支援此商店頁面格式，請複製診斷資訊'],
 PUBLIC_METADATA_FAILED:['Could not load public product information; retry later','公開の商品情報を取得できません。後で再試行してください','공개 상품 정보를 불러올 수 없습니다. 나중에 다시 시도하세요','无法加载公开商品信息，请稍后重试','無法載入公開商品資訊，請稍後重試'],
 PUBLIC_METADATA_INVALID:['Could not read public product information; copy diagnostics','公開の商品情報を読み取れません。診断情報をコピーしてください','공개 상품 정보를 읽을 수 없습니다. 진단 정보를 복사하세요','无法读取公开商品信息，请复制诊断信息','無法讀取公開商品資訊，請複製診斷資訊'],
 PUBLIC_METADATA_UNAVAILABLE:['The public product page is unavailable','公開の商品ページを利用できません','공개 상품 페이지를 사용할 수 없습니다','公开商品页面不可用','公開商品頁面無法使用'],
 RECEIPT_REQUEST_FAILED:['Could not load order details; retry','注文の明細を取得できません。再試行してください','주문 상세를 불러올 수 없습니다. 다시 시도하세요','无法加载订单明细，请重试','無法載入訂單明細，請重試'],
 RECEIPT_PARSE_FAILED:['Could not read order details; copy diagnostics','注文の明細を読み取れません。診断情報をコピーしてください','주문 상세를 읽을 수 없습니다. 진단 정보를 복사하세요','无法读取订单明细，请复制诊断信息','無法讀取訂單明細，請複製診斷資訊'],
 RECEIPT_BODY_LIMIT:['Order details exceeded the size limit; copy diagnostics','注文の明細がサイズ上限を超えました。診断情報をコピーしてください','주문 상세가 크기 제한을 초과했습니다. 진단 정보를 복사하세요','订单明细超出大小限制，请复制诊断信息','訂單明細超出大小限制，請複製診斷資訊'],
 PRIVATE_UNKNOWN_QUANTITY:['Order quantity could not be confirmed; copy diagnostics','注文数を確認できません。診断情報をコピーしてください','주문 수량을 확인할 수 없습니다. 진단 정보를 복사하세요','无法确认订单数量，请复制诊断信息','無法確認訂單數量，請複製診斷資訊'],
 PRIVATE_UNKNOWN_STATUS:['Order payment status could not be confirmed; copy diagnostics','注文の支払い状況を確認できません。診断情報をコピーしてください','주문 결제 상태를 확인할 수 없습니다. 진단 정보를 복사하세요','无法确认订单付款状态，请复制诊断信息','無法確認訂單付款狀態，請複製診斷資訊'],
 BOOTH_BINDING_UNAVAILABLE:['Could not match the store file to its purchase; retry sync','ストアのファイルを購入履歴と照合できません。同期を再試行してください','스토어 파일과 구매 내역을 연결할 수 없습니다. 동기화를 다시 시도하세요','无法将商店文件与购买记录对应，请重试同步','無法將商店檔案與購買紀錄對應，請重試同步'],
 ENTITLEMENT_INVALID:['Store download access could not be confirmed; retry sync','ストアのダウンロード権限を確認できません。同期を再試行してください','스토어 다운로드 권한을 확인할 수 없습니다. 동기화를 다시 시도하세요','无法确认商店下载权限，请重试同步','無法確認商店下載權限，請重試同步'],
 PUBLICATION_OR_FILES_FAILED:['Could not save all product or file information; retry sync','商品情報またはファイル情報を保存しきれません。同期を再試行してください','일부 상품 또는 파일 정보를 저장하지 못했습니다. 동기화를 다시 시도하세요','未能保存全部商品或文件信息，请重试同步','未能儲存全部商品或檔案資訊，請重試同步'],
 SOURCE_TRAVERSAL_LIMIT:['The store list exceeded a sync limit; copy diagnostics','ストアの一覧が同期の上限を超えました。診断情報をコピーしてください','스토어 목록이 동기화 제한을 초과했습니다. 진단 정보를 복사하세요','商店列表超出同步限制，请复制诊断信息','商店清單超出同步限制，請複製診斷資訊'],
 BOOTH_ALREADY_RUNNING:['BOOTH is already syncing; wait for it to finish','BOOTHは同期中です。完了するまでお待ちください','BOOTH 동기화가 진행 중입니다. 완료될 때까지 기다리세요','BOOTH 正在同步，请等待完成','BOOTH 正在同步，請等待完成'],
 ACCOUNT_UNAVAILABLE:['This account is unavailable; check Accounts in Settings','アカウントを利用できません。設定のアカウントを確認してください','이 계정을 사용할 수 없습니다. 설정의 계정을 확인하세요','此账户不可用，请检查设置中的账户','此帳號無法使用，請檢查設定中的帳號'],
 FREE_OFFER_UNAVAILABLE:['This free download is no longer available','この無料ダウンロードは提供を終了しています','이 무료 다운로드는 더 이상 제공되지 않습니다','此免费下载已停止提供','此免費下載已停止提供'],
 'catalogue.invalid_avatar_selection':['Selected avatar is no longer available','選択したアバターは利用できなくなりました','선택한 아바타를 더 이상 사용할 수 없습니다','所选素体已不可用','選取的素體已無法使用'],
 'catalogue.invalid_avatar_delta':['Avatar selection could not be saved','アバターの選択を保存できませんでした','아바타 선택을 저장할 수 없습니다','无法保存素体选择','無法儲存素體選擇'],
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
 LIBRARY_PERMISSION_DENIED:['Cannot write to this folder (for example, protected locations like Program Files). Choose a user folder or another writable drive.','このフォルダーに書き込めません（Program Filesなどの保護された場所）。ユーザーフォルダーか、書き込み可能な別のドライブを選択してください。','이 폴더에 쓸 수 없습니다(Program Files 등 보호된 위치). 사용자 폴더나 쓰기 가능한 다른 드라이브를 선택하세요.','无法写入此文件夹（如 Program Files 等受保护位置）。请选择用户文件夹或其他可写入的磁盘。','無法寫入此資料夾（如 Program Files 等受保護位置）。請選擇使用者資料夾或其他可寫入的磁碟。'],
 LIBRARY_DISK_FULL:['Not enough free space for the library. Free up space or choose another drive.','ライブラリの空き容量が不足しています。空き容量を増やすか、別のドライブを選択してください。','라이브러리 저장 공간이 부족합니다. 공간을 확보하거나 다른 드라이브를 선택하세요.','资产库可用空间不足。请释放空间或选择其他磁盘。','資產庫可用空間不足。請釋放空間或選擇其他磁碟。'],
 LIBRARY_PATH_NOT_DIRECTORY:['A file is blocking the selected folder path. Choose another folder.','選択したフォルダーのパスにファイルがあります。別のフォルダーを選択してください。','선택한 폴더 경로에 파일이 있습니다. 다른 폴더를 선택하세요.','所选文件夹路径被文件占用。请选择其他文件夹。','所選資料夾路徑被檔案占用。請選擇其他資料夾。'],
 LIBRARY_DATABASE_UNAVAILABLE:['Cannot open or write the library database. Check folder access and retry, or choose another location.','ライブラリのデータベースを開くか書き込むことができません。フォルダーのアクセス権を確認して再試行するか、別の保存先を選択してください。','라이브러리 데이터베이스를 열거나 쓸 수 없습니다. 폴더 접근 권한을 확인하고 다시 시도하거나 다른 위치를 선택하세요.','无法打开或写入资产库数据库。请检查文件夹访问权限后重试，或选择其他位置。','無法開啟或寫入資產庫資料庫。請檢查資料夾存取權限後重試，或選擇其他位置。'],
 LIBRARY_CREATE_FAILED:['Cannot create the library folder. Check the selected path and drive, then retry or choose another location.','ライブラリのフォルダーを作成できません。選択したパスとドライブを確認して再試行するか、別の保存先を選択してください。','라이브러리 폴더를 만들 수 없습니다. 선택한 경로와 드라이브를 확인하고 다시 시도하거나 다른 위치를 선택하세요.','无法创建资产库文件夹。请检查所选路径和磁盘后重试，或选择其他位置。','無法建立資產庫資料夾。請檢查所選路徑和磁碟後重試，或選擇其他位置。'],
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
 const lower=code.toLowerCase();
 const nativeCode=lower.split('.').at(-1)??lower;
 const upper=nativeCode.toUpperCase();
 const aliases:Record<string,string>={SOURCE_UNSUPPORTED:'COLLECTOR_UNSUPPORTED',RECEIPT_BODY_FAILED:'RECEIPT_REQUEST_FAILED',RECEIPT_AUTH_FAILED:'auth',RECEIPT_AUTHORIZATION_REQUIRED:'auth',PROTECTED_SESSION_UNAVAILABLE:'auth',DIGITAL_DETAIL_ROLE_UNKNOWN:'RECEIPT_PARSE_FAILED',GUMROAD_PUBLIC_METADATA_UNAVAILABLE:'PUBLIC_METADATA_UNAVAILABLE',FILE_INCOMPLETE:'manifest_incomplete',DOWNLOAD_LINK_UNAVAILABLE:'download_resolver_failed',DOWNLOAD_RESOLVER_FAILED:'download_resolver_failed'};
 let key=labels[code]?code:labels[upper]?upper:aliases[upper]??'';
 if(!key&&['store_identity_changed','store_identity_mismatch','account_identity_mismatch'].includes(nativeCode))key='auth';
 if(!key&&/^(jinxxy|gumroad)_(source_unsupported|scope_changed|pagination_unproven)$/.test(nativeCode))key='operationFailed';
 if(!key&&/^(file_unavailable|downloads?_unavailable)$/.test(nativeCode))key='missing';
 if(!key&&['collector_unsupported','unsupported_image'].includes(nativeCode))key='validation';
 if(!key&&['unsafe_download_url','unsafe_redirect','redirect_limit'].includes(nativeCode))key='operationFailed';
 if(!key&&/(?:^|[._])(?:auth|login|session)(?:[._]|$)|(?:^|[._])expired(?:[._]|$)/.test(lower))key='auth';
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
