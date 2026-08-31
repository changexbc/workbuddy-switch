export interface StackedSegmentVisualLayout {
  height: number;
  isTop: boolean;
  y: number;
}

function allocateVisualHeights(
  values: number[],
  pixelsPerValue: number,
  minHeight: number,
): number[] | null {
  if (!Number.isFinite(pixelsPerValue) || pixelsPerValue <= 0) return null;
  const rawHeights = values.map((value) => Math.max(0, value * pixelsPerValue));
  const nonZero = rawHeights.filter((height) => height > 0);
  if (nonZero.length === 0) return null;

  const heights = [...rawHeights];
  const minTotal = nonZero.length * minHeight;
  const rawTotal = rawHeights.reduce((sum, height) => sum + height, 0);
  if (rawTotal < minTotal) {
    return heights.map((height) => (height > 0 ? minHeight : 0));
  }

  let deficit = 0;
  let donorExcess = 0;
  for (const height of heights) {
    if (height > 0 && height < minHeight) deficit += minHeight - height;
    if (height > minHeight) donorExcess += height - minHeight;
  }
  if (deficit <= 0 || donorExcess <= 0) return heights;

  return heights.map((height) => {
    if (height <= 0) return 0;
    if (height < minHeight) return minHeight;
    return height - (deficit * (height - minHeight)) / donorExcess;
  });
}

export function getStackedSegmentVisualLayout({
  values,
  segmentIndex,
  segmentHeight,
  segmentY,
  stackStart,
  minHeight = 5,
}: {
  values: number[];
  segmentIndex: number;
  segmentHeight: number;
  segmentY: number;
  stackStart: number;
  minHeight?: number;
}): StackedSegmentVisualLayout | null {
  const safeValues = values.map((value) =>
    Number.isFinite(value) && value > 0 ? value : 0,
  );
  const segmentValue = safeValues[segmentIndex] ?? 0;
  if (segmentValue <= 0 || segmentHeight <= 0 || segmentIndex < 0) return null;

  const pixelsPerValue = segmentHeight / segmentValue;
  const heights = allocateVisualHeights(safeValues, pixelsPerValue, minHeight);
  if (!heights) return null;

  const baseline = segmentY + segmentHeight + Math.max(0, stackStart) * pixelsPerValue;
  const offsetBelow = heights
    .slice(0, segmentIndex)
    .reduce((sum, height) => sum + height, 0);
  const height = heights[segmentIndex] ?? 0;
  const topIndex = safeValues.reduce(
    (result, value, index) => (value > 0 ? index : result),
    -1,
  );

  return {
    height,
    isTop: segmentIndex === topIndex,
    y: baseline - offsetBelow - height,
  };
}
