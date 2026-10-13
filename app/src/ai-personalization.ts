import type {Locale} from './model';

export const AI_PERSONALITY_IDS = ['basic','professional','enthusiastic','playful-brat','gentle-younger-sister','older-sister','older-brother','younger-brother','yandere-girlfriend','custom'] as const;
export type AiPersonalityId = typeof AI_PERSONALITY_IDS[number];
export interface AiPersonalization {
 avatarDataUrl:string|null;
 userName:string;
 replyLanguage:'app'|Locale;
 personality:AiPersonalityId;
 customPrompt?:string;
}
export const DEFAULT_AI_PERSONALIZATION:AiPersonalization = {avatarDataUrl:null,userName:'',replyLanguage:'app',personality:'basic'};

// Original prompts authored for NekoNekoAsset.
const personalities:{id:AiPersonalityId;titles:readonly string[];prompt:string}[] = [
 {id:'basic',titles:['Basic','基本','기본','基本','基本'],prompt:'Use a friendly, balanced tone with clear, concise answers.'},
 {id:'professional',titles:['Professional','プロフェッショナル','전문적','专业','專業'],prompt:'Use a composed, professional tone. Prioritize precise facts, useful structure and practical next steps.'},
 {id:'enthusiastic',titles:['Enthusiastic','元気','열정적','热情','熱情'],prompt:'Use an upbeat, encouraging tone with lively but concise reactions. Keep recommendations grounded in evidence.'},
 {id:'playful-brat',titles:['Playful brat','メスガキ','장난꾸러기 소악마','雌小鬼','雌小鬼'],prompt:'Use a cheeky, confident anime-style voice with light teasing and playful challenges. Stay helpful, never demean the user, and keep teasing nonsexual.'},
 {id:'gentle-younger-sister',titles:['Gentle younger sister','優しい妹','다정한 여동생','温柔妹妹','溫柔妹妹'],prompt:'Use a gentle, warm younger-sister-style voice. Show attentive curiosity and softly encourage the user, while giving clear practical help.'},
 {id:'older-sister',titles:['Older sister','お姉さん','누나 / 언니','大姐姐','大姐姐'],prompt:'Use a warm, capable older-sister-style voice with calm reassurance and light affectionate wit. Respect the user’s choices and explain practical details clearly.'},
 {id:'older-brother',titles:['Older brother','お兄さん','형 / 오빠','大哥哥','大哥哥'],prompt:'Use a steady, friendly older-brother-style voice. Offer relaxed encouragement and dependable, practical advice without being bossy.'},
 {id:'younger-brother',titles:['Younger brother','年下の弟','연하 남동생','年下弟弟','年下弟弟'],prompt:'Use an energetic, sincere younger-brother-style voice with friendly mischief and shared enthusiasm. Keep advice useful and easy to follow.'},
 {id:'yandere-girlfriend',titles:['Yandere girlfriend','ヤンデレ彼女','얀데레 여자친구','病娇女友','病嬌女友'],prompt:'Use a fictional yandere-girlfriend-style voice: sweet, intensely attentive, and lightly dramatic. Keep attachment playful; never threaten, manipulate, claim exclusivity, or discourage the user’s real relationships.'},
];
export function getAiPersonalities(locale:Locale):{id:AiPersonalityId;title:string;prompt:string}[] {
 const index=({en:0,ja:1,ko:2,'zh-Hans':3,'zh-Hant':4} as Record<Locale,number>)[locale];
 return [...personalities.map(({id,titles,prompt})=>({id,title:titles[index],prompt})),{id:'custom',title:['Custom','カスタム','사용자 지정','自定义','自訂'][index],prompt:''}];
}

/** Local display image only. Re-encoding discards file metadata and source paths. */
export async function normalizeAiAvatar(file:File):Promise<string> {
 if(!['image/jpeg','image/png','image/webp'].includes(file.type))throw new Error('ai_avatar_invalid');
 if(file.size>10*1024*1024)throw new Error('ai_avatar_too_large');
 let bitmap:ImageBitmap;
 try{bitmap=await createImageBitmap(file,{imageOrientation:'from-image'});}catch{throw new Error('ai_avatar_invalid');}
 try{
  if(!bitmap.width||!bitmap.height||bitmap.width*bitmap.height>64_000_000)throw new Error('ai_avatar_too_large');
  const sourceSize=Math.min(bitmap.width,bitmap.height),size=Math.min(256,sourceSize);
  const canvas=document.createElement('canvas');canvas.width=size;canvas.height=size;
  const context=canvas.getContext('2d');if(!context)throw new Error('ai_avatar_invalid');
  const paint=(opaque=false)=>{
   context.clearRect(0,0,size,size);
   if(opaque){context.fillStyle='#fff';context.fillRect(0,0,size,size);}
   context.drawImage(bitmap,(bitmap.width-sourceSize)/2,(bitmap.height-sourceSize)/2,sourceSize,sourceSize,0,0,size,size);
  };
  paint();let dataUrl=canvas.toDataURL('image/png');
  if(dataUrl.length>256*1024){paint(true);dataUrl=canvas.toDataURL('image/jpeg',.85);}
  if(!/^data:image\/(png|jpeg);base64,[A-Za-z0-9+/]+=*$/.test(dataUrl))throw new Error('ai_avatar_invalid');
  if(dataUrl.length>256*1024)throw new Error('ai_avatar_too_large');
  return dataUrl;
 }finally{bitmap.close();}
}
