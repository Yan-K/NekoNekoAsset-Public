import {AlertCircle,ChartPie,CheckCircle2,Clock,Cloud,Code,Gem,Globe,Inbox,Layers,LoaderCircle,Palette,PersonStanding,Scissors,Shirt,User,WandSparkles,Wrench} from 'lucide-react';
import type {LucideIcon} from 'lucide-react';
import type {ProductDisplayStatus} from './model';

const statusIcons:Record<ProductDisplayStatus,LucideIcon>={notDownloaded:Cloud,partial:ChartPie,processing:LoaderCircle,ready:CheckCircle2,attention:AlertCircle,unknown:Clock};
const categoryIcons:Record<string,LucideIcon>={Base:User,Clothes:Shirt,Hair:Scissors,Accessory:Gem,Texture:Layers,Motion:PersonStanding,Tool:Wrench,Shader:Palette,Gimmick:WandSparkles,World:Globe,Udon:Code,NonVRChat:Inbox};

/** One glyph per display status, shared by cards, rows and the inspector. Processing spins. */
export function StatusIcon({status,size=12}:{status:ProductDisplayStatus;size?:number}){const Icon=statusIcons[status]??Clock;return <Icon size={size} aria-hidden="true" className={status==='processing'?'progress-spin':undefined}/>;}
export function CategoryIcon({category,size=15}:{category:string;size?:number}){const Icon=categoryIcons[category]??Inbox;return <Icon size={size} aria-hidden="true"/>;}
