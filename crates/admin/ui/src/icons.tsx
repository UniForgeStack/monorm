import { Activity, AlertTriangle, Archive, BarChart3, Bell, Book, BookOpen, Box, Boxes, Briefcase, Building2, Calendar, Clipboard, Clock, Cloud, Cog, CreditCard, Database, DollarSign, File, FileText, Flag, Folder, Gift, Globe, Heart, History, House, Image, Inbox, Key, Layers, LayoutDashboard, LifeBuoy, Link as LinkIcon, List, Lock, Mail, Map, MessageSquare, Package, Percent, Receipt, RefreshCw, Search, Server, Settings, Shield, ShoppingBag, ShoppingCart, Star, Store, Tag, Tags, Ticket, Truck, User, UserCog, Users, Wallet, Wrench, Zap, type LucideIcon } from "lucide-react";
import type { SVGProps } from "react";

/** The icons that a resource, a group or a service can name, by lucide name (kebab case). A name that is not here shows a plain dot. */
const ICONS: Record<string, LucideIcon> = {
  activity: Activity, "alert-triangle": AlertTriangle, archive: Archive, "bar-chart-3": BarChart3, bell: Bell, book: Book, "book-open": BookOpen, box: Box, boxes: Boxes,
  briefcase: Briefcase, "building-2": Building2, calendar: Calendar, clipboard: Clipboard, clock: Clock, cloud: Cloud, cog: Cog, "credit-card": CreditCard, database: Database,
  "dollar-sign": DollarSign, file: File, "file-text": FileText, flag: Flag, folder: Folder, gift: Gift, globe: Globe, heart: Heart, history: History, house: House,
  image: Image, inbox: Inbox, key: Key, layers: Layers, "layout-dashboard": LayoutDashboard, "life-buoy": LifeBuoy, link: LinkIcon, list: List, lock: Lock, mail: Mail, map: Map,
  "message-square": MessageSquare, package: Package, percent: Percent, receipt: Receipt, "refresh-cw": RefreshCw, search: Search, server: Server, settings: Settings,
  shield: Shield, "shopping-bag": ShoppingBag, "shopping-cart": ShoppingCart, star: Star, store: Store, tag: Tag, tags: Tags, ticket: Ticket, truck: Truck, user: User,
  "user-cog": UserCog, users: Users, wallet: Wallet, wrench: Wrench, zap: Zap,
};

/** The icon of a lucide name, or a plain dot for a name that the UI does not know (or no name: `fallback`). */
export function NavIcon({ name, fallback, ...props }: { name?: string | null; fallback?: LucideIcon } & Omit<SVGProps<SVGSVGElement>, "name">) {
  const Icon = (name ? ICONS[name] : undefined) ?? fallback;
  if (Icon) return <Icon aria-hidden {...(props as object)} />;
  return (
    <svg viewBox="0 0 24 24" aria-hidden {...props}>
      <circle cx="12" cy="12" r="3" fill="currentColor" />
    </svg>
  );
}
