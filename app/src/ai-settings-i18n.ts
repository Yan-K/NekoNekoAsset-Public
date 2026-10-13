import type {Locale} from './model';
import {nativeWorkflowText} from './native-workflow-i18n';
const words:Record<string,string[]>={
 entries:['AI configurations','AI設定','AI 구성','AI 配置','AI 設定'],
 emptyEntries:['No saved configuration','保存済み設定なし','저장된 구성 없음','尚无已保存配置','尚無已儲存設定'],
 addEntry:['Add','追加','추가','添加','新增'],
 draftTested:['Test passed','テスト成功','테스트 통과','测试通过','測試通過'],
 personalization:['Personalization','パーソナライズ','개인화','个性化','個人化'],
 avatar:['Avatar','アイコン','프로필 이미지','头像','頭像'],
 changeAvatar:['Change','変更','변경','更换','更換'],
 userName:['User name','ユーザー名','사용자 이름','用户名称','用戶名稱'],
 userNamePlaceholder:['You','あなた','나','你','你'],
 replyLanguage:['Reply language','返信言語','답변 언어','回复语言','回覆語言'],
 followAppLanguage:['Follow app language','アプリの言語に合わせる','앱 언어에 맞추기','跟随应用语言','跟隨 App 語言'],
 replyPersonality:['Reply personality','返信の性格','답변 성격','回复性格','回覆性格'],
 customPrompt:['Custom prompt','カスタム指示','사용자 지정 지침','自定义提示词','自訂提示詞'],
 personalizationSaved:['Saved','保存しました','저장됨','已保存','已儲存'],
 ai_avatar_invalid:['Choose a JPEG, PNG or WebP image.','JPEG、PNG、WebP画像を選んでください。','JPEG, PNG 또는 WebP 이미지를 선택하세요.','请选择 JPEG、PNG 或 WebP 图片。','請選擇 JPEG、PNG 或 WebP 圖片。'],
 ai_avatar_too_large:['This avatar image is too large.','このアイコン画像は大きすぎます。','프로필 이미지가 너무 큽니다.','头像图片过大。','頭像圖片過大。'],
 ai_personalization_invalid:['Check the personalization fields.','パーソナライズの項目を確認してください。','개인화 설정을 확인하세요.','请检查个性化设置。','請檢查個人化設定。'],
 ai_chat_storage:['Could not load or save AI data.','AIデータを読み込み・保存できませんでした。','AI 데이터를 불러오거나 저장하지 못했습니다.','无法读取或保存 AI 数据。','無法讀取或儲存 AI 資料。'],
 ai_network:['Could not connect to the selected AI provider.','選択したAIプロバイダーに接続できませんでした。','선택한 AI 공급자에 연결하지 못했습니다.','无法连接所选 AI 提供商。','無法連線至所選 AI 服務。'],
 ai_rate_limited:['AI request limit reached.','AIのリクエスト制限に達しました。','AI 요청 한도에 도달했습니다.','已达到 AI 请求频率限制。','已達 AI 請求頻率限制。'],
 ai_quota_exceeded:['AI provider quota exceeded.','AIプロバイダーの利用枠を超えました。','AI 공급자 사용 한도를 초과했습니다.','AI 提供商配额已用尽。','AI 服務配額已用盡。'],
 ai_spend_limit_exceeded:['AI provider spending limit reached.','AIプロバイダーの利用額上限に達しました。','AI 공급자 지출 한도에 도달했습니다.','已达到 AI 提供商支出上限。','已達 AI 服務支出上限。'],
 ai_timeout:['AI provider request timed out.','AIプロバイダーへのリクエストがタイムアウトしました。','AI 공급자 요청 시간이 초과되었습니다.','AI 提供商请求超时。','AI 服務請求逾時。'],
 ai_malformed:['Invalid AI provider response.','AIプロバイダーの応答形式が無効です。','AI 공급자 응답이 올바르지 않습니다.','AI 提供商响应格式无效。','AI 服務回應格式無效。'],
 ai_refused:['The AI provider refused the request.','AIプロバイダーがリクエストを拒否しました。','AI 공급자가 요청을 거부했습니다.','AI 提供商拒绝了请求。','AI 服務拒絕了請求。'],
 ai_unavailable:['The AI provider is temporarily unavailable.','AIプロバイダーは一時的に利用できません。','AI 공급자를 일시적으로 사용할 수 없습니다.','AI 提供商暂时不可用。','AI 服務暫時無法使用。'],
 ai_not_configured:['Select a saved AI configuration.','保存済みのAI設定を選択してください。','저장된 AI 구성을 선택하세요.','请选择已保存的 AI 配置。','請選擇已儲存的 AI 設定。'],
 ai_entry_missing:['This configuration is no longer available.','この設定は存在しません。','이 구성을 더 이상 사용할 수 없습니다.','此配置已不存在。','此設定已不存在。'],
 ai_entry_limit:['The configuration list is full.','設定リストが上限に達しました。','구성 목록이 가득 찼습니다.','配置列表已满。','設定清單已滿。'],
 ai_test_required:['Test this draft before saving.','保存前にこの設定をテストしてください。','저장 전에 이 초안을 테스트하세요.','请先测试此草稿再保存。','請先測試此草稿再儲存。'],
 ai_test_stale:['The draft changed or the test expired. Test again.','設定が変更されたかテストが期限切れです。再テストしてください。','초안이 변경되었거나 테스트가 만료되었습니다. 다시 테스트하세요.','草稿已更改或测试已过期，请重新测试。','草稿已變更或測試已過期，請重新測試。'],
};
export function aiSettingsText(locale:Locale,key:string):string {
 const index=({en:0,ja:1,ko:2,'zh-Hans':3,'zh-Hant':4} as Record<string,number>)[locale]??0;
 return words[key]?.[index]??nativeWorkflowText(locale,key);
}
