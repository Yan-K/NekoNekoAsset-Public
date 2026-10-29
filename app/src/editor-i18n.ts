import type { Locale } from './model';
import { t } from './i18n';
import { wf } from './workflow-i18n';

const labels = {
  en: { tagInput:'Find or create a tag', createTag:'Create tag', addTagChoice:'Add tag', removeTagChoice:'Remove tag', tagSuggestions:'Suggested tags', themeName:'Theme name', existingCollections:'Collections', newCollection:'New collection', chooseCollection:'Choose a collection', createAndAdd:'Create and add', addToExisting:'Add', collectionRequired:'Choose a collection or enter a new name.', collectionExists:'This collection already exists. Choose it from the list.', themeNameRequired:'Enter a theme name.' },
  ja: { tagInput:'タグを検索・作成', createTag:'タグを作成', addTagChoice:'タグを追加', removeTagChoice:'タグを削除', tagSuggestions:'タグ候補', themeName:'テーマ名', existingCollections:'コレクション', newCollection:'新しいコレクション', chooseCollection:'コレクションを選択', createAndAdd:'作成して追加', addToExisting:'追加', collectionRequired:'コレクションを選択するか、新しい名前を入力してください。', collectionExists:'同名のコレクションがあります。一覧から選択してください。', themeNameRequired:'テーマ名を入力してください。' },
  ko: { tagInput:'태그 검색 또는 만들기', createTag:'태그 만들기', addTagChoice:'태그 추가', removeTagChoice:'태그 제거', tagSuggestions:'추천 태그', themeName:'테마 이름', existingCollections:'컬렉션', newCollection:'새 컬렉션', chooseCollection:'컬렉션 선택', createAndAdd:'만들고 추가', addToExisting:'추가', collectionRequired:'컬렉션을 선택하거나 새 이름을 입력하세요.', collectionExists:'같은 이름의 컬렉션이 있습니다. 목록에서 선택하세요.', themeNameRequired:'테마 이름을 입력하세요.' },
  'zh-Hans': { tagInput:'查找或创建标签', createTag:'创建标签', addTagChoice:'添加标签', removeTagChoice:'移除标签', tagSuggestions:'标签建议', themeName:'主题名称', existingCollections:'收藏集', newCollection:'新建收藏集', chooseCollection:'选择收藏集', createAndAdd:'创建并添加', addToExisting:'添加', collectionRequired:'请选择收藏集或输入新名称。', collectionExists:'此收藏集已存在，请从列表中选择。', themeNameRequired:'请输入主题名称。' },
  'zh-Hant': { tagInput:'尋找或建立標籤', createTag:'建立標籤', addTagChoice:'加入標籤', removeTagChoice:'移除標籤', tagSuggestions:'標籤建議', themeName:'主題名稱', existingCollections:'收藏集', newCollection:'新增收藏集', chooseCollection:'選擇收藏集', createAndAdd:'建立並加入', addToExisting:'加入', collectionRequired:'請選擇收藏集或輸入新名稱。', collectionExists:'此收藏集已存在，請從清單中選擇。', themeNameRequired:'請輸入主題名稱。' },
} satisfies Record<Locale, Record<string, string>>;
const extra:Record<string,[string,string,string,string,string]>={
  uploadThumbnail:['Upload','アップロード','업로드','上传','上傳'],
  invalidThumbnail:['Choose a PNG, JPEG or WebP image up to 10 MB.','10 MB 以下の PNG・JPEG・WebP 画像を選択してください。','10 MB 이하의 PNG, JPEG 또는 WebP 이미지를 선택하세요.','请选择不超过 10 MB 的 PNG、JPEG 或 WebP 图片。','請選擇不超過 10 MB 的 PNG、JPEG 或 WebP 圖片。'],
  manageCollections:['Collections','コレクション','컬렉션','收藏集','收藏集'],
  createCollection:['Create collection','コレクションを作成','컬렉션 만들기','创建收藏集','建立收藏集'],
  collectionActionFailed:['Could not update the collection. Try again.','コレクションを更新できませんでした。もう一度お試しください。','컬렉션을 업데이트하지 못했습니다. 다시 시도하세요.','无法更新收藏集，请重试。','無法更新收藏集，請重試。'],
  manageSearches:['Saved searches','保存した検索','저장된 검색','已保存的搜索','已儲存的搜尋'],
  nameExists:['This name already exists.','この名前は既に使用されています。','이미 사용 중인 이름입니다.','此名称已存在。','此名稱已存在。'],
  rename:['Rename','名前を変更','이름 변경','重命名','重新命名'],
  delete:['Delete','削除','삭제','删除','刪除'],
  confirmDeleteCollection:['Delete collection “{name}”?','コレクション「{name}」を削除しますか？','컬렉션 “{name}”을 삭제할까요?','删除收藏集“{name}”？','刪除收藏集「{name}」？'],
  confirmDeleteSearch:['Delete saved search “{name}”?','保存した検索「{name}」を削除しますか？','저장된 검색 “{name}”을 삭제할까요?','删除已保存的搜索“{name}”？','刪除已儲存的搜尋「{name}」？'],
  confirmDeleteTheme:['Delete theme “{name}”?','テーマ「{name}」を削除しますか？','테마 “{name}”을 삭제할까요?','删除主题“{name}”？','刪除主題「{name}」？'],
  entryUnavailable:['This item is no longer available. Close and reopen the list.','この項目は利用できなくなりました。一覧を開き直してください。','이 항목은 더 이상 사용할 수 없습니다. 목록을 닫았다 다시 여세요.','此项目已不可用，请关闭并重新打开列表。','此項目已無法使用，請關閉並重新開啟清單。'],
  addEntry:['Add','追加','추가','添加','新增'],
  addingEntry:['Adding…','追加中…','추가 중…','正在添加…','正在新增…'],
  creatingEntry:['Creating…','作成中…','만드는 중…','正在创建…','正在建立…'],
  fetchProductData:['Fetch product data','商品情報を取得','상품 정보 가져오기','抓取商品资料','抓取商品資料'],
  fetchingProductData:['Fetching…','取得中…','가져오는 중…','正在抓取…','正在抓取…'],
  resettingThumbnail:['Resetting…','復元中…','복원 중…','正在重置…','正在重設…'],
  files:['Files','ファイル','파일','文件','檔案'],
  addFiles:['Add files','ファイルを追加','파일 추가','添加文件','新增檔案'],
  removeFile:['Remove','削除','제거','移除','移除'],
  restoreFile:['Keep','保持','유지','保留','保留'],
  editAvatars:['Avatars','アバター','아바타','素体','素體'],
  selectAllAvatars:['Select all','すべて選択','모두 선택','全选','全選'],
  ratingOn:['Enabled','有効','활성화','已启用','已啟用'],
  ratingOff:['Disabled','無効','비활성화','已停用','已停用'],
  avatarSearch:['Search avatars or shops','アバター・ショップを検索','아바타 또는 상점 검색','搜索素体或商店','搜尋素體或商店'],
  avatarNoResults:['No matching avatars','該当するアバターなし','일치하는 아바타 없음','没有符合的素体','沒有符合的素體'],
  editAvatar:['Edit avatar name and shop','アバター名・ショップ名を編集','아바타 이름 및 상점 편집','编辑素体名称和商店','編輯素體名稱和商店'],
  avatarName:['Avatar name','アバター名','아바타 이름','素体名称','素體名稱'],
  avatarShop:['Shop name','ショップ名','상점 이름','商店名称','商店名稱'],
  avatarEditFailed:['Could not save the avatar. Try again.','アバターを保存できませんでした。もう一度お試しください。','아바타를 저장하지 못했습니다. 다시 시도하세요.','无法保存素体，请重试。','無法儲存素體，請重試。'],
};
export function editorText(locale: Locale, key: string): string { return extra[key]?.[['en','ja','ko','zh-Hans','zh-Hant'].indexOf(locale)] ?? (labels[locale] as Record<string,string>)[key] ?? (['productURL','invalidURL'].includes(key)?wf(locale,key):t(locale,key)); }
