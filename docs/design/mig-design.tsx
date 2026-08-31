import React, { useState, useEffect, useRef } from 'react';
import { 
  User, Users, MessageSquare, Gift, Gamepad2, Bell, Mail, 
  Settings, HelpCircle, LogOut, ChevronDown, ChevronRight, 
  Search, RefreshCw, Volume2, VolumeX, Smartphone, Laptop, 
  Send, Smile, Shield, Crown, Play, CheckSquare, Square, X, PlusCircle, Sparkles
} from 'lucide-react';

// --- Web Audio Retro Sound Effects ---
const playRetroSound = (type) => {
  try {
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.connect(gain);
    gain.connect(ctx.destination);

    if (type === 'keypress') {
      osc.type = 'square';
      osc.frequency.setValueAtTime(800, ctx.currentTime);
      gain.gain.setValueAtTime(0.05, ctx.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.05);
      osc.start();
      osc.stop(ctx.currentTime + 0.05);
    } else if (type === 'msg') {
      osc.type = 'sine';
      osc.frequency.setValueAtTime(523.25, ctx.currentTime); // C5
      osc.frequency.setValueAtTime(659.25, ctx.currentTime + 0.08); // E5
      osc.frequency.setValueAtTime(783.99, ctx.currentTime + 0.16); // G5
      gain.gain.setValueAtTime(0.1, ctx.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.3);
      osc.start();
      osc.stop(ctx.currentTime + 0.3);
    } else if (type === 'dice') {
      osc.type = 'triangle';
      osc.frequency.setValueAtTime(300, ctx.currentTime);
      osc.frequency.linearRampToValueAtTime(150, ctx.currentTime + 0.15);
      gain.gain.setValueAtTime(0.1, ctx.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.15);
      osc.start();
      osc.stop(ctx.currentTime + 0.15);
    }
  } catch (e) {
    // Audio Context restricted before interaction
  }
};

export default function App() {
  // Device Layout Mode: 'mobile' (Clean Screen without bezel) or 'desktop' (Spiral Notebook skin)
  const [deviceMode, setDeviceMode] = useState('mobile');
  const [soundEnabled, setSoundEnabled] = useState(true);

  // Authentication State
  const [isLoggedIn, setIsLoggedIn] = useState(false);
  const [username, setUsername] = useState('reason007007');
  const [password, setPassword] = useState('••••••••');
  const [rememberMe, setRememberMe] = useState(true);
  const [loginInvisible, setLoginInvisible] = useState(false);
  const [autoLogin, setAutoLogin] = useState(false);

  // Dynamic Tabs State
  const initialSystemTabs = [
    { id: 'friends', title: 'My Friends', type: 'system', icon: 'users' },
    { id: 'rooms', title: 'Rooms', type: 'system', icon: 'rooms' },
    { id: 'games', title: 'Games', type: 'system', icon: 'games' },
    { id: 'updates', title: 'Feed', type: 'system', icon: 'feed' },
  ];
  const [openTabs, setOpenTabs] = useState(initialSystemTabs);
  const [activeTabId, setActiveTabId] = useState('friends');

  // Profile data
  const [statusText, setStatusText] = useState('Euphoria Whisper');
  const [userStatus, setUserStatus] = useState('Available');
  const [eggCount, setEggCount] = useState(24);
  const [credits, setCredits] = useState(5000);

  // Chat Histories Map: { tabId: [messages...] }
  const [chatHistories, setChatHistories] = useState({});
  const [chatInput, setChatInput] = useState('');
  const [showEmoticonPicker, setShowEmoticonPicker] = useState(false);

  // Friends & Rooms Data
  const [friends, setFriends] = useState([
    { id: 1, name: 'reason008', status: 'online', isVip: true, mood: 'Main dice yuk!' },
    { id: 2, name: 'nrock', status: 'online', isVip: false, mood: 'Listening to Linkin Park' },
    { id: 3, name: 'neel_the_great', status: 'online', isVip: true, mood: 'Salam kawan' },
    { id: 4, name: 'ahok', status: 'online', isVip: false, mood: 'Ada yang mau barter egg?' },
    { id: 5, name: 'sampit_gaul', status: 'offline', isVip: false, mood: 'tidur dulu zzz' }
  ]);

  const [chatRooms, setChatRooms] = useState([
    { id: 'r1', name: 'sampit_terindah', users: 9, max: 30, category: 'Recent Rooms' },
    { id: 'r2', name: 'indo terindah', users: 14, max: 40, category: 'Recent Rooms' },
    { id: 'r3', name: 'malang.jomblo2', users: 12, max: 40, category: 'Recent Rooms' },
    { id: 'r4', name: 'Jakarta_Gaul', users: 22, max: 50, category: 'Favorites' },
    { id: 'r5', name: 'Cari_Jodoh_Nusantara', users: 35, max: 50, category: 'Favorites' }
  ]);

  // Dice Game State
  const [diceState, setDiceState] = useState({
    playerRoll: null,
    botRoll: null,
    result: null,
    bet: 100,
    isPlaying: false
  });

  // Emoticons
  const emoticons = [
    { code: ':D', symbol: '😃' },
    { code: ';P', symbol: '😜' },
    { code: '(bot)', symbol: '🤖' },
    { code: '(egg)', symbol: '🥚' },
    { code: '(gift)', symbol: '🎁' },
    { code: '(kiss)', symbol: '💋' },
    { code: '(devil)', symbol: '😈' },
    { code: '(heart)', symbol: '❤️' }
  ];

  const chatBottomRef = useRef(null);

  // Auto scroll chat
  useEffect(() => {
    chatBottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [chatHistories, activeTabId]);

  const playFx = (type) => {
    if (soundEnabled) playRetroSound(type);
  };

  // Handle Login
  const handleLogin = (e) => {
    if (e) e.preventDefault();
    playFx('keypress');
    setIsLoggedIn(true);
    if (loginInvisible) setUserStatus('Invisible');
  };

  // Handle Logout
  const handleLogout = () => {
    playFx('keypress');
    setIsLoggedIn(false);
    setOpenTabs(initialSystemTabs);
    setActiveTabId('friends');
  };

  // Open Chat Room in New Tab
  const handleOpenRoomTab = (room) => {
    playFx('keypress');
    const tabId = `room-${room.id}`;
    
    // Check if tab is already open
    const exists = openTabs.find(t => t.id === tabId);
    if (!exists) {
      const newTab = {
        id: tabId,
        title: `#${room.name}`,
        type: 'chatroom',
        roomData: room,
        closable: true
      };
      setOpenTabs(prev => [...prev, newTab]);

      // Initialize chat history if empty
      if (!chatHistories[tabId]) {
        setChatHistories(prev => ({
          ...prev,
          [tabId]: [
            { id: 1, sender: 'System', text: `*** Selamat datang di room ${room.name} ***`, isSystem: true },
            { id: 2, sender: 'Bot', text: 'Halo gaes! Jaga kesopanan & patuhi rule room ya.', isBot: true },
            { id: 3, sender: 'reason008', text: `Halo @${username}! Baru bergabung?`, time: '11:32' },
            { id: 4, sender: 'sampit_gaul', text: 'ada yang jual ID cantik ga?', time: '11:33' }
          ]
        }));
      }
    }
    setActiveTabId(tabId);
  };

  // Open PM in New Tab
  const handleOpenPMTab = (friendName) => {
    playFx('keypress');
    const tabId = `pm-${friendName}`;

    // Check if tab is already open
    const exists = openTabs.find(t => t.id === tabId);
    if (!exists) {
      const newTab = {
        id: tabId,
        title: `@${friendName}`,
        type: 'pm',
        targetName: friendName,
        closable: true
      };
      setOpenTabs(prev => [...prev, newTab]);

      // Initialize PM history
      if (!chatHistories[tabId]) {
        setChatHistories(prev => ({
          ...prev,
          [tabId]: [
            { id: 1, sender: 'System', text: `Sesi obrolan pribadi dengan ${friendName}`, isSystem: true },
            { id: 2, sender: friendName, text: 'Oi bro, lagi di mana?', time: '11:30' }
          ]
        }));
      }
    }
    setActiveTabId(tabId);
  };

  // Close Tab
  const handleCloseTab = (e, tabIdToClose) => {
    e.stopPropagation();
    playFx('keypress');

    const nextTabs = openTabs.filter(t => t.id !== tabIdToClose);
    setOpenTabs(nextTabs);

    if (activeTabId === tabIdToClose) {
      setActiveTabId(nextTabs[nextTabs.length - 1]?.id || 'friends');
    }
  };

  // Send Message in Active Tab
  const handleSendMessage = (e) => {
    e.preventDefault();
    if (!chatInput.trim()) return;
    playFx('keypress');

    const activeTab = openTabs.find(t => t.id === activeTabId);
    if (!activeTab) return;

    const newMsg = {
      id: Date.now(),
      sender: username,
      text: chatInput,
      time: new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    };

    setChatHistories(prev => ({
      ...prev,
      [activeTabId]: [...(prev[activeTabId] || []), newMsg]
    }));
    setChatInput('');
    setShowEmoticonPicker(false);

    // Auto Response Simulation
    setTimeout(() => {
      playFx('msg');
      let replyMsg;
      if (activeTab.type === 'chatroom') {
        replyMsg = {
          id: Date.now() + 1,
          sender: 'reason008',
          text: `Mantap Bro! 🥚 ${chatInput.includes(':D') ? '😀' : ''}`,
          time: new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
        };
      } else if (activeTab.type === 'pm') {
        replyMsg = {
          id: Date.now() + 1,
          sender: activeTab.targetName,
          text: 'Sip mas, nanti aku kirim gift telur ya!',
          time: new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
        };
      }
      if (replyMsg) {
        setChatHistories(prev => ({
          ...prev,
          [activeTabId]: [...(prev[activeTabId] || []), replyMsg]
        }));
      }
    }, 1200);
  };

  // Throw Egg
  const handleSendEgg = () => {
    if (eggCount <= 0) return;
    playFx('msg');
    setEggCount(prev => prev - 1);

    const activeTab = openTabs.find(t => t.id === activeTabId);
    const targetText = activeTab?.title || 'Room';

    setChatHistories(prev => ({
      ...prev,
      [activeTabId]: [
        ...(prev[activeTabId] || []),
        {
          id: Date.now(),
          sender: 'System',
          text: `🥚 ${username} melempar TELUR KELUARGA ke ${targetText}! (+10 Exp)`,
          isSystem: true
        }
      ]
    }));
  };

  // Play Dice Game
  const handleRollDice = () => {
    if (credits < diceState.bet) {
      alert('Kredit tidak cukup! Silakan topup credit.');
      return;
    }
    playFx('dice');
    setDiceState(prev => ({ ...prev, isPlaying: true }));

    setTimeout(() => {
      const pRoll = Math.floor(Math.random() * 6) + 1;
      const bRoll = Math.floor(Math.random() * 6) + 1;
      let res = 'SERI';
      let change = 0;

      if (pRoll > bRoll) {
        res = 'MENANG';
        change = diceState.bet;
      } else if (pRoll < bRoll) {
        res = 'KALAH';
        change = -diceState.bet;
      }

      setCredits(prev => prev + change);
      setDiceState({
        playerRoll: pRoll,
        botRoll: bRoll,
        result: res,
        bet: diceState.bet,
        isPlaying: false
      });
      playFx('msg');
    }, 800);
  };

  const activeTabObject = openTabs.find(t => t.id === activeTabId) || openTabs[0];

  return (
    <div className="min-h-screen bg-slate-900 text-slate-100 flex flex-col items-center justify-start p-2 sm:p-6 font-sans select-none">
      
      {/* Header Bar Switcher */}
      <header className="w-full max-w-4xl bg-slate-800 border border-slate-700 rounded-xl p-3 mb-4 flex flex-wrap items-center justify-between gap-3 shadow-lg">
        <div className="flex items-center gap-2">
          <div className="w-8 h-8 bg-cyan-500 rounded-full flex items-center justify-center font-bold text-white shadow">
            <MessageSquare className="w-5 h-5 text-white" />
          </div>
          <div>
            <h1 className="text-base font-bold text-cyan-400 flex items-center gap-1.5">
              Nostalgia Simulator
              <span className="text-[10px] bg-orange-500/20 text-orange-400 px-2 py-0.5 rounded-full border border-orange-500/30">
                Multi-Tab Edition
              </span>
            </h1>
            <p className="text-xs text-slate-400">Replikasi Presisi UI Chat & Tab Obrolan</p>
          </div>
        </div>

        {/* Mode & Sound Controls */}
        <div className="flex items-center gap-2 bg-slate-900/80 p-1.5 rounded-lg border border-slate-700">
          <button
            onClick={() => { playFx('keypress'); setDeviceMode('mobile'); }}
            className={`flex items-center gap-1.5 text-xs px-3 py-1.5 rounded-md font-medium transition ${
              deviceMode === 'mobile' ? 'bg-cyan-600 text-white shadow' : 'text-slate-400 hover:text-white'
            }`}
          >
            <Smartphone className="w-4 h-4" /> Mode Mobile
          </button>
          <button
            onClick={() => { playFx('keypress'); setDeviceMode('desktop'); }}
            className={`flex items-center gap-1.5 text-xs px-3 py-1.5 rounded-md font-medium transition ${
              deviceMode === 'desktop' ? 'bg-cyan-600 text-white shadow' : 'text-slate-400 hover:text-white'
            }`}
          >
            <Laptop className="w-4 h-4" /> Mode Desktop
          </button>
          <button
            onClick={() => { playFx('keypress'); setSoundEnabled(!soundEnabled); }}
            className="p-1.5 text-slate-400 hover:text-cyan-400 rounded-md"
            title="Toggle Retro Sound Effects"
          >
            {soundEnabled ? <Volume2 className="w-4 h-4 text-emerald-400" /> : <VolumeX className="w-4 h-4 text-slate-500" />}
          </button>
        </div>
      </header>

      {/* MAIN APPLICATION CONTAINER (Tanpa Bingkai HP) */}
      <div className="w-full max-w-xl bg-[#fdfbf7] text-slate-900 rounded-2xl border-2 border-slate-700 shadow-2xl flex flex-col min-h-[620px] max-h-[720px] overflow-hidden relative font-sans text-xs">
        
        {!isLoggedIn ? (
          /* LOGIN SCREEN */
          <div className="flex-1 bg-gradient-to-b from-[#0093AF] via-[#00ACC1] to-[#00838F] flex flex-col items-center justify-between p-6 text-white relative overflow-hidden">
            
            <div className="w-full flex justify-between items-center text-[10px] text-cyan-100 font-mono mb-2 z-10">
              <span className="flex items-center gap-1">📶 TELKOMSEL 3G</span>
              <span>11:33 🔋</span>
            </div>

            <div className="flex flex-col items-center my-auto z-10 w-full max-w-xs">
              <div className="relative mb-3">
                <div className="bg-[#00BCD4] border-2 border-white text-white px-6 py-3 rounded-3xl font-extrabold text-3xl shadow-lg tracking-wider flex items-center justify-center gap-2">
                  <MessageSquare className="w-8 h-8 text-white" />
                  <span>Chat</span>
                </div>
              </div>

              <div className="flex items-end justify-center gap-1 mb-4">
                <div className="w-7 h-7 bg-emerald-400 border border-white rounded-t-full flex items-center justify-center text-xs">🤖</div>
                <div className="w-9 h-9 bg-slate-100 border border-slate-300 rounded-t-full flex items-center justify-center text-sm shadow">🤖</div>
                <div className="w-7 h-7 bg-pink-400 border border-white rounded-t-full flex items-center justify-center text-xs">🌸</div>
              </div>

              <p className="text-xs font-medium text-cyan-100 mb-4 tracking-wide">Join the Fun!</p>

              <form onSubmit={handleLogin} className="w-full space-y-2.5">
                <div>
                  <input
                    type="text"
                    value={username}
                    onChange={(e) => setUsername(e.target.value)}
                    placeholder="Username"
                    className="w-full px-3 py-2 text-slate-800 bg-white border border-cyan-200 rounded shadow-inner focus:outline-none text-xs"
                    required
                  />
                </div>
                <div className="relative">
                  <input
                    type="password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    placeholder="Password"
                    className="w-full px-3 py-2 text-slate-800 bg-white border border-cyan-200 rounded shadow-inner focus:outline-none text-xs"
                    required
                  />
                  <button type="button" className="absolute right-2 top-2 text-cyan-600 font-bold text-xs">?</button>
                </div>

                <button
                  type="submit"
                  className="w-full py-2.5 bg-gradient-to-r from-orange-500 to-amber-500 hover:from-orange-600 hover:to-amber-600 text-white font-bold rounded shadow-md border border-orange-300 active:scale-95 transition text-xs tracking-wider"
                >
                  Go!
                </button>

                <div className="flex items-center justify-center gap-1.5 pt-1 text-[11px] text-cyan-50">
                  <input
                    type="checkbox"
                    id="invisibleCheck"
                    checked={loginInvisible}
                    onChange={(e) => setLoginInvisible(e.target.checked)}
                    className="rounded border-cyan-300 accent-orange-500"
                  />
                  <label htmlFor="invisibleCheck" className="cursor-pointer">Login as Invisible</label>
                </div>
              </form>
            </div>

            <div className="z-10 pb-2">
              <button onClick={() => alert('Fitur Registrasi')} className="text-cyan-100 text-xs underline font-semibold hover:text-white">
                Create Account
              </button>
            </div>

          </div>
        ) : (
          /* MAIN APPLICATION VIEW WITH DYNAMIC TABS */
          <div className="flex-1 flex flex-col bg-[#fdfbf7] text-slate-900 overflow-hidden">
            
            {/* DYNAMIC SCROLLABLE TAB BAR */}
            <div className="bg-[#00838F] text-white flex items-center text-[11px] font-semibold border-b border-cyan-900 shadow-sm overflow-x-auto no-scrollbar scroll-smooth">
              {openTabs.map((tab) => {
                const isActive = activeTabId === tab.id;
                return (
                  <div
                    key={tab.id}
                    onClick={() => { playFx('keypress'); setActiveTabId(tab.id); }}
                    className={`py-1.5 px-3 flex items-center gap-1.5 border-r border-cyan-700/60 cursor-pointer whitespace-nowrap transition shrink-0 ${
                      isActive ? 'bg-[#00ACC1] text-white font-bold border-b-2 border-orange-400' : 'hover:bg-cyan-800 text-cyan-100'
                    }`}
                  >
                    {tab.type === 'system' && tab.id === 'friends' && <Users className="w-3.5 h-3.5 text-cyan-200" />}
                    {tab.type === 'system' && tab.id === 'rooms' && <MessageSquare className="w-3.5 h-3.5 text-cyan-200" />}
                    {tab.type === 'system' && tab.id === 'games' && <Gamepad2 className="w-3.5 h-3.5 text-cyan-200" />}
                    {tab.type === 'system' && tab.id === 'updates' && <Sparkles className="w-3.5 h-3.5 text-cyan-200" />}
                    
                    {tab.type === 'chatroom' && <span className="text-amber-300 font-bold">💬</span>}
                    {tab.type === 'pm' && <span className="text-emerald-300 font-bold">👤</span>}

                    <span>{tab.title}</span>

                    {/* Close Tab Button */}
                    {tab.closable && (
                      <button
                        onClick={(e) => handleCloseTab(e, tab.id)}
                        className="ml-1 p-0.5 hover:bg-cyan-900 rounded-full text-cyan-200 hover:text-white"
                        title="Tutup Tab"
                      >
                        <X className="w-3 h-3" />
                      </button>
                    )}
                  </div>
                );
              })}
            </div>

            {/* USER PROFILE ORANGE BANNER */}
            <div className="bg-gradient-to-r from-orange-600 via-orange-500 to-amber-500 text-white p-2 flex items-center gap-2 border-b border-orange-600 shadow-inner">
              <div className="w-10 h-10 bg-white rounded border border-orange-300 p-0.5 flex items-center justify-center shadow shrink-0">
                <div className="w-full h-full bg-cyan-50 border border-cyan-200 rounded flex items-center justify-center text-lg">
                  🤖
                </div>
              </div>

              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-1.5">
                  <span className="w-2.5 h-2.5 bg-emerald-400 rounded-full border border-emerald-200 shadow-sm inline-block"></span>
                  <span className="font-bold text-xs truncate leading-none">{username}</span>
                </div>
                <p className="text-[10px] text-orange-100 truncate italic mt-0.5">{statusText}</p>
              </div>

              <div className="bg-orange-700/40 border border-orange-300/40 rounded px-2 py-1 text-center shrink-0 flex items-center gap-2">
                <div className="flex items-center gap-0.5 text-[10px] font-bold text-amber-200">
                  <span>🥚</span>
                  <span>{eggCount}</span>
                </div>
                <div className="text-[10px] font-bold text-emerald-200">
                  Rp {credits.toLocaleString()}
                </div>
              </div>
            </div>

            {/* TAB CONTENT AREA */}
            <div className="flex-1 overflow-y-auto bg-white flex flex-col">

              {/* 1. FRIENDS TAB */}
              {activeTabId === 'friends' && (
                <div className="divide-y divide-slate-100">
                  <div className="p-2 flex items-center gap-2 bg-white hover:bg-slate-50 cursor-pointer">
                    <Mail className="w-4 h-4 text-cyan-600" />
                    <span className="text-xs font-medium text-slate-800">Email</span>
                    <span className="text-xs text-slate-500 font-mono">(0)</span>
                  </div>

                  <div className="p-2 flex items-center gap-2 bg-white hover:bg-slate-50 cursor-pointer">
                    <div className="w-4 h-4 bg-orange-500 text-white rounded font-bold text-[10px] flex items-center justify-center shadow-sm">
                      !
                    </div>
                    <span className="text-xs font-bold text-orange-600">Updates</span>
                    <span className="text-xs font-bold text-orange-600 font-mono">(9)</span>
                  </div>

                  <div className="bg-slate-100 px-2 py-1 text-[10px] font-bold text-slate-500 uppercase tracking-wider flex justify-between items-center">
                    <span>Online Friends ({friends.filter(f=>f.status==='online').length})</span>
                    <button onClick={() => playFx('keypress')} className="text-cyan-700 hover:underline">Refresh</button>
                  </div>

                  {friends.filter(f=>f.status==='online').map((friend) => (
                    <div
                      key={friend.id}
                      onClick={() => handleOpenPMTab(friend.name)}
                      className="p-2.5 flex items-center gap-2 hover:bg-cyan-50/70 cursor-pointer border-b border-slate-100 transition"
                    >
                      <span className="w-2.5 h-2.5 bg-emerald-500 rounded-full border border-emerald-200 shrink-0"></span>
                      <div className="flex-1 min-w-0">
                        <div className="flex items-center gap-1">
                          <span className="font-semibold text-xs text-slate-800">{friend.name}</span>
                          {friend.isVip && <Crown className="w-3 h-3 text-amber-500 fill-amber-400" />}
                        </div>
                        <p className="text-[10px] text-slate-500 truncate">{friend.mood}</p>
                      </div>
                      <button className="text-[10px] bg-cyan-600 text-white px-2 py-0.5 rounded font-medium shadow-sm hover:bg-cyan-700">
                        Buka Tab PM
                      </button>
                    </div>
                  ))}

                  <div className="mt-2 space-y-0.5">
                    {['Facebook (0/0)', 'MSN (0/0)', 'Yahoo! (0/0)', 'GTalk (0/0)'].map((im, idx) => (
                      <div key={idx} className="bg-[#E0F7FA] border-y border-cyan-100 px-2 py-1.5 text-xs text-cyan-900 font-medium flex items-center gap-2 hover:bg-cyan-100 cursor-pointer">
                        <span className="font-bold text-cyan-700">–</span>
                        <span>{im}</span>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {/* 2. CHAT ROOMS TAB */}
              {activeTabId === 'rooms' && (
                <div className="divide-y divide-slate-100">
                  <div className="p-2 bg-slate-50 border-b border-slate-200 flex items-center gap-1">
                    <Search className="w-3.5 h-3.5 text-slate-400" />
                    <input
                      type="text"
                      placeholder="Search Chatrooms..."
                      className="w-full text-xs bg-white border border-slate-300 rounded px-2 py-1 focus:outline-none"
                    />
                  </div>

                  <div className="bg-cyan-50/80 px-2 py-1 text-[10px] font-bold text-cyan-800 border-b border-cyan-100 flex items-center gap-1">
                    <ChevronDown className="w-3 h-3" />
                    <span>Favorites ({chatRooms.filter(r=>r.category==='Favorites').length})</span>
                  </div>
                  {chatRooms.filter(r=>r.category==='Favorites').map((room) => (
                    <div
                      key={room.id}
                      onClick={() => handleOpenRoomTab(room)}
                      className="p-2.5 flex items-center justify-between hover:bg-cyan-50 cursor-pointer border-b border-slate-100"
                    >
                      <div className="flex items-center gap-2">
                        <span className="text-cyan-700">💬</span>
                        <span className="font-semibold text-xs text-slate-800">{room.name}</span>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-[10px] text-slate-500 font-mono">({room.users} / {room.max})</span>
                        <span className="text-[10px] bg-amber-500 text-white px-2 py-0.5 rounded font-bold shadow-sm">Masuk Tab Room</span>
                      </div>
                    </div>
                  ))}

                  <div className="bg-cyan-50/80 px-2 py-1 text-[10px] font-bold text-cyan-800 border-b border-cyan-100 flex items-center gap-1">
                    <ChevronDown className="w-3 h-3" />
                    <span>Recent Rooms ({chatRooms.filter(r=>r.category==='Recent Rooms').length})</span>
                  </div>
                  {chatRooms.filter(r=>r.category==='Recent Rooms').map((room) => (
                    <div
                      key={room.id}
                      onClick={() => handleOpenRoomTab(room)}
                      className="p-2.5 flex items-center justify-between hover:bg-cyan-50 cursor-pointer border-b border-slate-100"
                    >
                      <div className="flex items-center gap-2">
                        <span className="text-cyan-700">💬</span>
                        <span className="font-semibold text-xs text-slate-800">{room.name}</span>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className="text-[10px] text-slate-500 font-mono">({room.users} / {room.max})</span>
                        <span className="text-[10px] bg-amber-500 text-white px-2 py-0.5 rounded font-bold shadow-sm">Masuk Tab Room</span>
                      </div>
                    </div>
                  ))}

                  <div className="p-2 text-center">
                    <button onClick={() => alert('Fitur Buat Room')} className="text-xs text-cyan-700 font-bold hover:underline flex items-center justify-center gap-1 w-full py-1.5 bg-cyan-50 border border-cyan-200 rounded">
                      <PlusCircle className="w-3.5 h-3.5" /> Create New Room
                    </button>
                  </div>
                </div>
              )}

              {/* 3. GAMES TAB */}
              {activeTabId === 'games' && (
                <div className="p-3 space-y-3">
                  <div className="bg-gradient-to-r from-amber-500 to-orange-500 text-white p-2.5 rounded shadow-sm text-xs">
                    <p className="font-bold">🎲 Game Zone</p>
                    <p className="text-[10px] opacity-90">Main game & kumpulkan Credit!</p>
                  </div>

                  {/* Dice Game Interactive Widget */}
                  <div className="bg-white p-4 rounded-xl border border-slate-200 text-center shadow-sm space-y-3">
                    <h3 className="font-extrabold text-sm text-slate-800 flex items-center justify-center gap-1.5">
                      🎲 Game Dice 10
                    </h3>

                    <div className="flex justify-around items-center py-2">
                      <div className="flex flex-col items-center">
                        <span className="text-xs font-bold text-slate-600 mb-1">{username}</span>
                        <div className="w-12 h-12 bg-gradient-to-br from-amber-400 to-orange-500 rounded-lg flex items-center justify-center text-xl text-white font-black shadow border-2 border-white">
                          {diceState.playerRoll !== null ? diceState.playerRoll : '?'}
                        </div>
                      </div>

                      <span className="font-black text-slate-400 text-sm">VS</span>

                      <div className="flex flex-col items-center">
                        <span className="text-xs font-bold text-slate-600 mb-1">Bot</span>
                        <div className="w-12 h-12 bg-gradient-to-br from-cyan-500 to-blue-600 rounded-lg flex items-center justify-center text-xl text-white font-black shadow border-2 border-white">
                          {diceState.botRoll !== null ? diceState.botRoll : '?'}
                        </div>
                      </div>
                    </div>

                    {diceState.result && (
                      <div className={`p-2 rounded font-extrabold text-xs ${
                        diceState.result === 'MENANG' ? 'bg-emerald-100 text-emerald-800 border border-emerald-300' :
                        diceState.result === 'KALAH' ? 'bg-rose-100 text-rose-800 border border-rose-300' : 'bg-slate-100 text-slate-800'
                      }`}>
                        {diceState.result === 'MENANG' && `🎉 Menang +Rp ${diceState.bet}!`}
                        {diceState.result === 'KALAH' && `😭 Kalah -Rp ${diceState.bet}`}
                        {diceState.result === 'SERI' && '🤝 Seri!'}
                      </div>
                    )}

                    <div className="space-y-2">
                      <div className="flex items-center justify-center gap-2 text-xs">
                        <span className="text-slate-600">Taruhan:</span>
                        {[100, 500, 1000].map((amt) => (
                          <button
                            key={amt}
                            onClick={() => setDiceState(p => ({ ...p, bet: amt }))}
                            className={`px-2 py-0.5 rounded text-xs font-bold ${
                              diceState.bet === amt ? 'bg-orange-500 text-white' : 'bg-slate-200 text-slate-700'
                            }`}
                          >
                            {amt}
                          </button>
                        ))}
                      </div>

                      <button
                        onClick={handleRollDice}
                        disabled={diceState.isPlaying}
                        className="w-full py-2 bg-gradient-to-r from-orange-500 to-amber-500 text-white font-extrabold rounded-lg shadow border border-orange-300 active:scale-95 disabled:opacity-50 text-xs tracking-wider"
                      >
                        {diceState.isPlaying ? 'Mengocok Dadu...' : 'LEMPAR DADU!'}
                      </button>
                    </div>
                  </div>
                </div>
              )}

              {/* 4. UPDATES / FEED TAB */}
              {activeTabId === 'updates' && (
                <div className="p-3 space-y-2">
                  <div className="bg-cyan-50 p-2.5 border border-cyan-200 rounded text-xs space-y-1">
                    <p className="font-bold text-cyan-900">Update Status Kamu:</p>
                    <div className="flex gap-1">
                      <input
                        type="text"
                        value={statusText}
                        onChange={(e) => setStatusText(e.target.value)}
                        className="flex-1 text-xs border border-cyan-300 rounded px-2 py-1 bg-white"
                      />
                      <button onClick={() => playFx('msg')} className="bg-cyan-700 text-white px-2.5 py-1 rounded font-bold text-[10px]">
                        Post
                      </button>
                    </div>
                  </div>

                  <div className="divide-y divide-slate-100 border border-slate-200 rounded bg-white">
                    <div className="p-2.5 text-xs">
                      <div className="flex justify-between items-center font-bold text-slate-800">
                        <span className="text-cyan-700">@reason008</span>
                        <span className="text-[9px] text-slate-400">10m ago</span>
                      </div>
                      <p className="text-slate-600 mt-1">Lagi seru nih di chatroom sampit_terindah, gabung yuk! 🥚🎁</p>
                    </div>
                    <div className="p-2.5 text-xs">
                      <div className="flex justify-between items-center font-bold text-slate-800">
                        <span className="text-cyan-700">@neel_the_great</span>
                        <span className="text-[9px] text-slate-400">1h ago</span>
                      </div>
                      <p className="text-slate-600 mt-1">Status: Euphoria Whisper ON 🎵</p>
                    </div>
                  </div>
                </div>
              )}

              {/* 5. ACTIVE CHATROOM OR PM TAB CONTENT */}
              {(activeTabObject.type === 'chatroom' || activeTabObject.type === 'pm') && (
                <div className="flex-1 flex flex-col bg-white">
                  
                  {/* Chat Sub-Header */}
                  <div className="bg-slate-100 px-3 py-1.5 border-b border-slate-200 flex items-center justify-between text-xs">
                    <div className="flex items-center gap-1.5 min-w-0 font-bold text-slate-800">
                      <span>{activeTabObject.title}</span>
                      <span className="text-[10px] text-emerald-600 font-mono">(Terhubung)</span>
                    </div>

                    <button
                      onClick={handleSendEgg}
                      className="bg-amber-500 hover:bg-amber-600 text-white font-bold px-2.5 py-1 rounded text-[10px] flex items-center gap-1 shadow-sm"
                    >
                      <span>🥚 Lempar Telur</span>
                    </button>
                  </div>

                  {/* Chat Messages Box */}
                  <div className="flex-1 p-3 overflow-y-auto space-y-2 bg-[#fdfcfa]">
                    {(chatHistories[activeTabId] || []).map((msg) => (
                      <div key={msg.id} className="text-xs">
                        {msg.isSystem ? (
                          <p className="text-[10px] text-amber-700 bg-amber-50 p-1.5 border border-amber-200 rounded text-center italic">
                            {msg.text}
                          </p>
                        ) : msg.isBot ? (
                          <div className="bg-emerald-50 border border-emerald-200 rounded p-2">
                            <span className="font-bold text-emerald-700 text-[11px]">🤖 {msg.sender}: </span>
                            <span className="text-slate-800">{msg.text}</span>
                          </div>
                        ) : (
                          <div className="flex flex-col">
                            <div className="flex items-baseline justify-between">
                              <span className={`font-bold text-[11px] ${msg.sender === username ? 'text-orange-600' : 'text-cyan-800'}`}>
                                {msg.sender}:
                              </span>
                              {msg.time && <span className="text-[9px] text-slate-400 font-mono">{msg.time}</span>}
                            </div>
                            <p className="text-slate-800 bg-slate-100/90 p-2 rounded border border-slate-200/80 mt-0.5">
                              {msg.text}
                            </p>
                          </div>
                        )}
                      </div>
                    ))}
                    <div ref={chatBottomRef} />
                  </div>

                  {/* Emoticon Picker Popup */}
                  {showEmoticonPicker && (
                    <div className="bg-slate-100 p-2 border-t border-slate-300 grid grid-cols-4 gap-1.5">
                      {emoticons.map((emo, i) => (
                        <button
                          key={i}
                          type="button"
                          onClick={() => {
                            setChatInput(prev => prev + ' ' + emo.code);
                            setShowEmoticonPicker(false);
                          }}
                          className="bg-white p-1 rounded border border-slate-300 hover:bg-cyan-50 flex items-center justify-center gap-1 text-xs"
                        >
                          <span>{emo.symbol}</span>
                          <span className="text-[9px] text-slate-500">{emo.code}</span>
                        </button>
                      ))}
                    </div>
                  )}

                  {/* Chat Input Form */}
                  <form onSubmit={handleSendMessage} className="p-2 bg-slate-200 border-t border-slate-300 flex items-center gap-1.5">
                    <button
                      type="button"
                      onClick={() => setShowEmoticonPicker(!showEmoticonPicker)}
                      className="p-1.5 bg-white border border-slate-300 rounded text-slate-700 hover:text-cyan-600"
                    >
                      <Smile className="w-4 h-4" />
                    </button>

                    <input
                      type="text"
                      value={chatInput}
                      onChange={(e) => setChatInput(e.target.value)}
                      placeholder="Ketik pesan..."
                      className="flex-1 text-xs px-2.5 py-1.5 bg-white border border-slate-300 rounded focus:outline-none"
                    />

                    <button
                      type="submit"
                      className="px-3.5 py-1.5 bg-cyan-700 hover:bg-cyan-800 text-white font-bold rounded text-xs"
                    >
                      Kirim
                    </button>
                  </form>

                </div>
              )}

            </div>

            {/* BOTTOM MENU FOOTER BUTTONS */}
            <div className="bg-[#006064] text-white border-t border-cyan-800 p-1.5 flex justify-between items-center text-[11px] font-bold">
              <button
                onClick={() => { playFx('keypress'); alert('Menu Options'); }}
                className="px-3 py-1 bg-cyan-800/80 hover:bg-cyan-700 rounded border border-cyan-600"
              >
                Options
              </button>

              <button
                onClick={handleLogout}
                className="px-3 py-1 bg-cyan-800/80 hover:bg-cyan-700 rounded border border-cyan-600 text-amber-200"
              >
                Exit / Logout
              </button>
            </div>

          </div>
        )}

      </div>

      <footer className="mt-4 text-center text-xs text-slate-500">
        <p>Nostalgia Simulator - Multi-tab chatroom & PM secara bersamaan.</p>
      </footer>

    </div>
  );
}