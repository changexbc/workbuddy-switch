import { SettingsForm } from './components/SettingsForm';
import { installStandaloneWindowChrome } from './host.js';
import { createRoot } from 'react-dom/client';
import '@/styles/tailwind.css';

// Must run before React mounts so the drag strip stays the first child of <body>.
installStandaloneWindowChrome();

const container = document.getElementById('root');
if (container) createRoot(container).render(<SettingsForm />);
