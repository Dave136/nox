export type Step = { keyframes: Record<string, string>; duration: number };

export const ISLAND = {
  compactWidth: 224,
  compactHeight: 52,
  expandedWidth: 354,
  top: 18,
  gutter: 20,
  scrollThreshold: 12,
  revealDuration: 0.22,
} as const;

export function expandedWidthFor(viewportWidth: number): number {
  return Math.min(ISLAND.expandedWidth, viewportWidth - ISLAND.gutter * 2);
}

export function openSequence(width: number, height: number): Step[] {
  return [
    { keyframes: { width: `${width}px` }, duration: 0.18 },
    { keyframes: { height: `${height}px` }, duration: 0.24 },
  ];
}

export function closeSequence(): Step[] {
  return [
    { keyframes: { height: `${ISLAND.compactHeight}px` }, duration: 0.22 },
    { keyframes: { width: `${ISLAND.compactWidth}px` }, duration: 0.18 },
  ];
}
