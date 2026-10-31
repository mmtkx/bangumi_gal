import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import App from './App';
import './styles.css';

const container = document.getElementById('root');
if (!container) {
  throw new Error('挂载点 #root 缺失，index.html 被改动过');
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
