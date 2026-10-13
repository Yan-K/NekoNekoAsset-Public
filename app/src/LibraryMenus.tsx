import {useEffect,useLayoutEffect,useRef,useState} from 'react';
import type {ReactNode} from 'react';
import {createPortal} from 'react-dom';
import {SlidersHorizontal} from 'lucide-react';

export interface ContextAction {id:string;label:string;icon?:ReactNode;disabled?:boolean;danger?:boolean;onClick:()=>void}
export function LibraryContextMenu({x,y,actions,onClose}:{x:number;y:number;actions:ContextAction[];onClose:()=>void}){
 const ref=useRef<HTMLDivElement>(null),previousFocus=useRef(document.activeElement),closeRef=useRef(onClose),[position,setPosition]=useState({left:x,top:y});closeRef.current=onClose;
 useLayoutEffect(()=>{const rect=ref.current?.getBoundingClientRect();if(rect)setPosition({left:Math.max(8,Math.min(x,innerWidth-rect.width-8)),top:Math.max(8,Math.min(y,innerHeight-rect.height-8))});ref.current?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();},[x,y]);
 useEffect(()=>{const previous=previousFocus.current;const close=(event:globalThis.PointerEvent)=>{if(!ref.current?.contains(event.target as Node))closeRef.current();};window.addEventListener('pointerdown',close,true);const blur=()=>closeRef.current();window.addEventListener('blur',blur);return()=>{window.removeEventListener('pointerdown',close,true);window.removeEventListener('blur',blur);if(previous instanceof HTMLElement&&previous.isConnected)previous.focus({preventScroll:true});};},[]);
 return createPortal(<div ref={ref} role="menu" className="library-context popup-menu" style={position} onContextMenu={event=>event.preventDefault()} onKeyDown={event=>{if(event.key==='Escape'){event.stopPropagation();event.preventDefault();onClose();}if(['ArrowDown','ArrowUp','Home','End'].includes(event.key)){event.preventDefault();const buttons=[...event.currentTarget.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')],index=buttons.indexOf(document.activeElement as HTMLButtonElement);buttons[event.key==='Home'?0:event.key==='End'?buttons.length-1:(index+(event.key==='ArrowDown'?1:-1)+buttons.length)%buttons.length]?.focus();}}}>{actions.map(action=><button role="menuitem" key={action.id} className={action.danger?'danger':undefined} disabled={action.disabled} onClick={()=>{onClose();action.onClick();}}>{action.icon}<span>{action.label}</span></button>)}</div>,document.querySelector('.app')??document.body);
}

export function ResponsiveLibraryToolbar({accounts,controls,chips,label}:{accounts:ReactNode;controls:ReactNode;chips:ReactNode;label:string}){
 const root=useRef<HTMLDivElement>(null),account=useRef<HTMLDivElement>(null),measure=useRef<HTMLDivElement>(null),menu=useRef<HTMLDetailsElement>(null),[collapsed,setCollapsed]=useState(false);
 useLayoutEffect(()=>{const calculate=()=>{if(root.current&&account.current&&measure.current){const children=[...measure.current.children] as HTMLElement[],needed=children.reduce((sum,child)=>sum+(child.classList.contains('toolbar-spacer')?8:child.getBoundingClientRect().width),0)+Math.max(0,children.length-1)*8;setCollapsed(root.current.clientWidth<account.current.getBoundingClientRect().width+needed+36);}};const observer=new ResizeObserver(calculate);for(const element of [root.current,account.current,measure.current])if(element)observer.observe(element);calculate();return()=>observer.disconnect();},[controls,accounts]);
 useEffect(()=>{const close=(event:globalThis.PointerEvent)=>{if(menu.current&&!menu.current.contains(event.target as Node))menu.current.open=false;};document.addEventListener('pointerdown',close);return()=>document.removeEventListener('pointerdown',close);},[]);
 return <div className="toolbar revised-toolbar library-toolbar-008" ref={root} data-collapsed={collapsed}>
  <div className="toolbar-account" ref={account}>{accounts}</div>
  <div ref={measure} className={'toolbar-control-group '+(collapsed?'toolbar-measure':'')} aria-hidden={collapsed||undefined}>{controls}</div>
  {collapsed&&<details className="toolbar-options" ref={menu} onKeyDown={event=>{if(event.key==='Escape'){event.stopPropagation();event.currentTarget.open=false;event.currentTarget.querySelector<HTMLElement>('summary')?.focus();}}}><summary aria-label={label} title={label}><SlidersHorizontal size={15}/><span>{label}</span></summary><div className="popup-menu">{controls}</div></details>}
  <div className="active-filter-chips">{chips}</div>
 </div>;
}
