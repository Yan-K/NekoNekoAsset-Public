import {useId} from 'react';
import './toggle.css';
export interface ToggleProps {label:string;checked:boolean;onChange:(checked:boolean)=>void;disabled?:boolean;id?:string;}
export default function Toggle({label,checked,onChange,disabled=false,id}:ToggleProps){
 const generated=useId(); const labelId=(id??generated)+'-label';
 return <span className="nna-toggle"><span id={labelId}>{label}</span><button id={id} type="button" role="switch" aria-checked={checked} aria-labelledby={labelId} disabled={disabled} onClick={()=>onChange(!checked)}><span aria-hidden="true"/></button></span>;
}
