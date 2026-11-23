import React from 'react';
import ReactDOM from 'react-dom/client';
import { ConfigProvider } from 'antd';
import zhCN from 'antd/locale/zh_CN';
import App from './App';
import { pmTheme, installGlobalStyles } from './theme';

installGlobalStyles();

const root = ReactDOM.createRoot(document.getElementById('root'));
root.render(
  <React.StrictMode>
    <ConfigProvider theme={pmTheme} locale={zhCN}>
      <App />
    </ConfigProvider>
  </React.StrictMode>
);
