import { Route, Routes, useLocation } from 'react-router-dom'
import Fade from './components/common/Fade'
import NavBar from './components/layout/NavBar'
import SettingsSidebar from './components/layout/SettingsSidebar'
import SimulatePage from './pages/SimulatePage'
import TeamsPage from './pages/TeamsPage'
import FormatsPage from './pages/FormatsPage'

export default function App() {
  const location = useLocation()
  return (
    <div className="flex h-screen flex-col">
      <NavBar />
      {/* The window never scrolls — pages scroll inside main (themed scrollbar). */}
      <main className="min-h-0 flex-1 overflow-y-auto">
        <Fade fadeKey={location.pathname} className="h-full">
          <Routes>
            <Route path="/" element={<TeamsPage />} />
            <Route path="/formats" element={<FormatsPage />} />
            <Route path="/simulate" element={<SimulatePage />} />
          </Routes>
        </Fade>
      </main>
      <SettingsSidebar />
    </div>
  )
}
