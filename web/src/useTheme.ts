import { ref } from 'vue';

export type Theme = 'dark' | 'light';

const THEME_KEY = 'oma_theme';

function apply(theme: Theme) {
  document.documentElement.dataset.theme = theme;
}

const stored = localStorage.getItem(THEME_KEY) === 'light' ? 'light' : 'dark';
export const theme = ref<Theme>(stored);

apply(stored);

export function toggleTheme() {
  theme.value = theme.value === 'dark' ? 'light' : 'dark';
  localStorage.setItem(THEME_KEY, theme.value);
  apply(theme.value);
}
