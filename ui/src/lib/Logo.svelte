<script lang="ts">
  // The app's mark: three isolinear chips, as on its icon.
  let { size = 64 }: { size?: number } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 64 64" aria-hidden="true">
  <rect x="4" y="4" width="56" height="56" rx="14" fill="#14213a" />
  <g transform="rotate(-14 32 32)">
    <rect x="15" y="13" width="9" height="40" rx="4.5" fill="#2bb3a3" />
    <rect x="27.5" y="11" width="9" height="40" rx="4.5" fill="#f2a33a" />
    <rect x="40" y="9" width="9" height="40" rx="4.5" fill="#9b8cf2" />
    <path d="M17.5 19h4M17.5 23h2.5M30 17h4M30 21h2.5M42.5 15h4M42.5 19h2.5" stroke="#14213a" stroke-width="1.8" stroke-linecap="round" />
  </g>
</svg>
