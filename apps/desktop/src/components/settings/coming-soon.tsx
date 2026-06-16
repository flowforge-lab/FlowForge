/** Placeholder body for sections whose content lands in a later SET issue. */
export function ComingSoon({ label }: { label: string }) {
  return (
    <div className="flex h-full min-h-40 flex-col items-center justify-center gap-1 text-center">
      <p className="text-[13px] font-medium text-foreground">{label}</p>
      <p className="text-[12px] text-muted-foreground">Coming soon.</p>
    </div>
  );
}
