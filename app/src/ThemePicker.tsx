import type { CSSProperties } from 'react';
import { Check } from 'lucide-react';
import type { Locale } from './model';
import { isLightTheme } from './model';
import { wf } from './workflow-i18n';
import './theme-picker.css';

export type ThemeOption = { id:string; name:string; tokens:Record<string,string> };
export type ThemePickerProps = { locale:Locale; options:ThemeOption[]; value:string; onChange:(id:string)=>void };
const tokenKeys=['bg','panel','raised','border','text','muted','accent','secondary-accent','secondary-accent-text','control','positive','warning','negative'];
const builtIn=(id:string,name:string,values:string[]):ThemeOption=>({id,name,tokens:Object.fromEntries(tokenKeys.map((key,index)=>['--'+key,values[index]]))});
/** Built-in themes use only the 13 exported theme tokens, so JSON export/import keeps working. */
export const defaultThemeOptions:ThemeOption[] = [
 {id:'dark',name:'Dark',tokens:{'--bg':'#171717','--panel':'#202020','--raised':'#292929','--border':'#373737','--text':'#ededed','--muted':'#a1a1a1','--accent':'#b6daca','--secondary-accent':'#8bb8f0','--secondary-accent-text':'#171717','--control':'#303030','--positive':'#b6daca','--warning':'#dfc689','--negative':'#e49c9c'}},
 {id:'light',name:'Light',tokens:{'--bg':'#f1f1f1','--panel':'#fafafa','--raised':'#ffffff','--border':'#d5d5d5','--text':'#252525','--muted':'#696969','--accent':'#426f5d','--secondary-accent':'#356cae','--secondary-accent-text':'#ffffff','--control':'#e9e9e9','--positive':'#386d55','--warning':'#896514','--negative':'#ad4343'}},
 builtIn('noir','Noir',['#000000','#0d0d0d','#171717','#2b2b2b','#f4f4f4','#a3a3a3','#ececec','#9ec4ff','#000000','#1f1f1f','#9fd6b2','#e8cb85','#f2a39d']),
 builtIn('midnight','Midnight',['#0e1222','#151a2e','#1d233b','#2f3758','#e9ecfb','#a5add2','#a9b9ff','#f3b884','#0e1222','#262d4a','#9fdcc0','#efd08c','#f5a3b0']),
 builtIn('ocean','Ocean',['#0b171c','#102128','#172b34','#29434f','#e3f1f5','#9dbac4','#7fe0d4','#ffb39b','#0b171c','#1c3540','#9ce0b5','#ecd28e','#f8a5a5']),
 builtIn('forest','Forest',['#111814','#17201a','#1f2a23','#33443a','#e7efe9','#a3b6a9','#bfe29a','#91c7f2','#111814','#26332b','#bfe29a','#e9cf88','#f2a69c']),
 builtIn('sakura','Sakura',['#1c1519','#261c22','#31242c','#4f3a46','#f8ecf2','#cbaebd','#f6b3cf','#9fc6f3','#1c1519','#3c2c36','#acdcbd','#efcf8f','#ff9f8f']),
 builtIn('neon','Neon',['#0c0a14','#14111f','#1c1829','#342c4a','#f0ebff','#aea6cd','#5ff0e5','#ff7fd1','#0c0a14','#252038','#8ff0ac','#ffd47e','#ff8fa0']),
 builtIn('ink','Ink',['#efefef','#fafafa','#ffffff','#d2d2d2','#171717','#5c5c5c','#1c1c1c','#2f5fc4','#ffffff','#e5e5e5','#2e6a47','#835e0e','#b03838']),
 builtIn('contrast','High contrast',['#e9e9e9','#ffffff','#ffffff','#5a5a5a','#000000','#2f2f2f','#0039a6','#6a1b9a','#ffffff','#e0e0e0','#0a5a2a','#7a4000','#a3001b']),
 builtIn('lemon','Lemon',['#f6f3e2','#fdfbee','#ffffff','#dcd4b0','#2b2a1e','#605c43','#2c4987','#86399a','#ffffff','#eee8ca','#386840','#875a09','#ad3636']),
 builtIn('peach','Peach',['#fbf0e9','#fff8f3','#ffffff','#e5c9b8','#3d2a22','#75574a','#1c686c','#6646ad','#ffffff','#f4e2d6','#2f6845','#875310','#b03838']),
 builtIn('lilac','Lilac',['#f2eff9','#faf8ff','#ffffff','#cec4e5','#2a2340','#5d5277','#6446b6','#1c7486','#ffffff','#e7e1f5','#2f6a4e','#875a12','#ad3349'])
];
// Display order; the seeded Mint/Violet/Sky/Amber/Rose/Silver presets join their tone group while they exist.
const groupOrder={dark:['dark','noir','silver','midnight','ocean','forest','amber','sakura','violet','neon'],light:['light','ink','contrast','sky','mint','lemon','peach','rose','lilac']};
const labels:Record<string,Record<Locale,string>>={
 contrast:{en:'High contrast',ja:'ハイコントラスト',ko:'고대비','zh-Hans':'高对比度','zh-Hant':'高對比'},
 custom:{en:'Custom',ja:'カスタム',ko:'사용자 지정','zh-Hans':'自定义','zh-Hant':'自訂'}
};
export function themeDisplayName(locale:Locale,option:ThemeOption):string{return ['dark','light'].includes(option.id)?wf(locale,option.id):labels[option.id]?.[locale]??option.name;}
function previewTokens(tokens:Record<string,string>):CSSProperties {
 const defaults=defaultThemeOptions[0].tokens;
 return Object.fromEntries(['bg','panel','raised','border','text','accent','secondary-accent'].map(key=>{
  const value=tokens['--'+key]??tokens[key];
  return ['--preview-'+key,typeof value==='string'&&/^#[a-f0-9]{6}$/i.test(value)?value:defaults['--'+key]];
 })) as CSSProperties;
}
function themeGroups(options:ThemeOption[]){
 const byId=new Map(options.map(option=>[option.id,option])),ordered=new Set<string>([...groupOrder.dark,...groupOrder.light]);
 const pick=(ids:string[])=>ids.flatMap(id=>byId.get(id)??[]);
 return [{key:'dark',items:pick(groupOrder.dark)},{key:'light',items:pick(groupOrder.light)},{key:'custom',items:options.filter(option=>!ordered.has(option.id))}].filter(group=>group.items.length);
}
export default function ThemePicker({locale,options,value,onChange}:ThemePickerProps) {
 const groups=themeGroups(options),flat=groups.flatMap(group=>group.items),selectedIndex=flat.findIndex(option=>option.id===value);
 return <div className="theme-picker" role="radiogroup" aria-label={wf(locale,'theme')}>
  {groups.map(group=><div className="theme-picker-group" key={group.key}>
   <div className="theme-picker-heading" aria-hidden="true"><span>{group.key==='custom'?labels.custom[locale]:wf(locale,group.key)}</span><small>{group.items.length}</small></div>
   <div className="theme-picker-grid">{group.items.map(option=>{const index=flat.indexOf(option),name=themeDisplayName(locale,option),checked=value===option.id;return <button type="button" key={option.id} className="nna-theme-option" data-tone={isLightTheme(option.tokens)?'light':'dark'} role="radio" aria-checked={checked} aria-label={name} title={name} tabIndex={checked||(selectedIndex<0&&index===0)?0:-1} onClick={()=>onChange(option.id)} onKeyDown={event=>{const offset=event.key==='ArrowRight'||event.key==='ArrowDown'?1:event.key==='ArrowLeft'||event.key==='ArrowUp'?-1:0;if(!offset&&event.key!=='Home'&&event.key!=='End')return;event.preventDefault();const next=event.key==='Home'?0:event.key==='End'?flat.length-1:(index+offset+flat.length)%flat.length;onChange(flat[next].id);event.currentTarget.closest('.theme-picker')?.querySelectorAll<HTMLButtonElement>('[role="radio"]')[next]?.focus();}}>
    <span className="theme-miniature" style={previewTokens(option.tokens)} aria-hidden="true"><i className="theme-mini-nav"/><span className="theme-mini-panel"><span className="theme-mini-actions"><b/><u/></span><em/><em/></span></span>
    <span className="theme-option-name"><span>{name}</span>{checked&&<Check size={13} aria-hidden="true"/>}</span>
   </button>;})}</div>
  </div>)}
 </div>;
}
