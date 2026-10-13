import type {Locale} from './model';
const text:Record<Locale,Record<string,string>>={
 'zh-Hant':{reset:'初始化',resetDelete:'初始化並刪除',resetMessage:'重設所有 App 設定，保留資料庫與素材。App 將重新啟動並進入首次設定。',deleteMessage:'重設所有 App 設定，永久刪除目前的整個資料庫資料夾，包含所有素材。App 將重新啟動並進入首次設定。',retained:'裝置上的登入憑證、API 金鑰與聊天紀錄會保留。',wait:'確認（{seconds} 秒）',restarting:'正在初始化並重新啟動…',deleteTheme:'刪除主題',busy:'請先等待或取消進行中的工作。'},
 'zh-Hans':{reset:'初始化',resetDelete:'初始化并删除',resetMessage:'重置所有 App 设置，保留数据库与素材。App 将重新启动并进入首次设置。',deleteMessage:'重置所有 App 设置，永久删除当前的整个数据库文件夹，包含所有素材。App 将重新启动并进入首次设置。',retained:'设备上的登录凭证、API 密钥与聊天记录会保留。',wait:'确认（{seconds} 秒）',restarting:'正在初始化并重新启动…',deleteTheme:'删除主题',busy:'请先等待或取消进行中的任务。'},
 en:{reset:'Reset',resetDelete:'Reset and delete',resetMessage:'Reset all App settings and keep the database and assets. The App will restart into first-time setup.',deleteMessage:'Reset all App settings and permanently delete the entire current database folder, including all assets. The App will restart into first-time setup.',retained:'Device sign-in credentials, API keys and chat history will be kept.',wait:'Confirm ({seconds}s)',restarting:'Resetting and restarting…',deleteTheme:'Delete theme',busy:'Wait for or cancel active jobs first.'},
 ja:{reset:'初期化',resetDelete:'初期化して削除',resetMessage:'App の全設定を初期化し、データベースと素材は保持します。App は再起動して初回設定に戻ります。',deleteMessage:'App の全設定を初期化し、現在のデータベースフォルダー全体とすべての素材を完全に削除します。App は再起動して初回設定に戻ります。',retained:'端末のログイン認証情報、API キー、チャット履歴は保持します。',wait:'確認（{seconds}秒）',restarting:'初期化して再起動中…',deleteTheme:'テーマを削除',busy:'実行中の処理が終わるまで待つか、キャンセルしてください。'},
 ko:{reset:'초기화',resetDelete:'초기화 및 삭제',resetMessage:'모든 App 설정을 초기화하고 데이터베이스와 에셋은 유지합니다. App이 다시 시작되어 최초 설정 화면으로 돌아갑니다.',deleteMessage:'모든 App 설정을 초기화하고 모든 에셋을 포함한 현재 데이터베이스 폴더 전체를 영구 삭제합니다. App이 다시 시작되어 최초 설정 화면으로 돌아갑니다.',retained:'기기의 로그인 인증 정보, API 키, 채팅 기록은 유지됩니다.',wait:'확인 ({seconds}초)',restarting:'초기화 및 다시 시작 중…',deleteTheme:'테마 삭제',busy:'진행 중인 작업을 기다리거나 취소하세요.'}
};
export const resetText=(locale:Locale,key:string)=>text[locale][key]??text.en[key]??key;
const errors:Record<string,string[]>={
 reset_incomplete:['Reset could not finish. Close other Apps or files using this database folder, then retry.','初期化を完了できません。このフォルダーを使用する App やファイルを閉じて再試行してください。','초기화를 완료하지 못했습니다. 이 폴더를 사용하는 App이나 파일을 닫고 다시 시도하세요.','初始化未能完成。请关闭使用此数据库文件夹的其他 App 或文件，然后重试。','初始化未能完成。請關閉使用此資料庫資料夾的其他 App 或檔案，再重試。'],
 reset_unsafe_location:['This database location cannot be safely reset.','この保存先を安全に初期化できません。','이 데이터베이스 위치를 안전하게 초기화할 수 없습니다.','无法安全初始化此数据库位置。','無法安全初始化此資料庫位置。'],
 reset_pending:['A reset is pending. Retry it before opening another database.','初期化が未完了です。別のデータベースを開く前に再試行してください。','초기화가 대기 중입니다. 다른 데이터베이스를 열기 전에 다시 시도하세요.','初始化尚未完成，请先重试，再打开其他数据库。','初始化尚未完成，請先重試，再開啟其他資料庫。'],
 reset_invalid_request:['The reset could not be safely verified.','初期化操作の安全性を確認できません。','초기화 작업을 안전하게 확인할 수 없습니다.','无法安全验证初始化操作。','無法安全驗證初始化操作。']
};
export const resetErrorText=(locale:Locale,code:string)=>errors[code]?.[(['en','ja','ko','zh-Hans','zh-Hant'] as Locale[]).indexOf(locale)];
