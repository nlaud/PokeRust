import { Route, Routes } from 'react-router-dom'
import NavBar from './components/layout/NavBar'
import SettingsSidebar from './components/layout/SettingsSidebar'
import BenchmarkingPage from './pages/BenchmarkingPage'
import SimulatePage from './pages/SimulatePage'
import TeamsPage from './pages/TeamsPage'
import FormatsPage from './pages/FormatsPage'
import TrackerPage from './pages/TrackerPage'

export default function App() {
  return (
    <div className="flex h-screen flex-col">
      <NavBar />
      {/* Keep window scrolling disabled. Scroll each page inside `main`. */}
      <main className="min-h-0 flex-1 overflow-y-auto">
        <Routes>
          <Route path="/" element={<TeamsPage />} />
          <Route path="/formats" element={<FormatsPage />} />
          <Route path="/simulate" element={<SimulatePage />} />
          <Route path="/tracker" element={<TrackerPage />} />
          <Route path="/benchmark" element={<BenchmarkingPage />} />
        </Routes>
      </main>
      <SettingsSidebar />
    </div>
  )
}
