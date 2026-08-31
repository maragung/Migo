Kode program memakai bahasa English.

Migo adalah platform komunikasi dan social community modern yang terinspirasi dari konsep messenger komunitas klasik, tetapi dibangun ulang dari nol dengan arsitektur modern, hemat bandwidth, aman, scalable, multi-region, dan mendukung Public Room, Managed Room, private chat, group chat, social system, virtual economy, mini games, serta game bots. Nama produk: Migo.

0. STATUS DOKUMEN DAN KONVENSI

Dokumen ini adalah product brief dan architecture brief Migo yang bersifat normatif.

Nomor section 1 sampai 135 dibekukan. Dokumen lain di dalam repository merujuk ke section dokumen ini dengan notasi "brief §NN", sehingga penomoran tidak boleh diubah. Penambahan requirement baru dilakukan dengan menambah section di akhir dokumen, bukan dengan menyisipkan nomor baru di tengah.

Pembagian tanggung jawab dokumen:

Section 1 sampai 135
Product requirement, arsitektur, fitur, dan operasional.

Section 136 sampai 178
Protocol specification normatif: envelope, encoding, packet type, negotiation, lifecycle, security model, call signaling, voice note, media, federation, bandwidth target, testing, observability, deployment, dan migration.

Section 179 dan seterusnya
Requirement produk yang ditambahkan setelah section 1 sampai 135 dibekukan. Isinya sederajat dengan section 1 sampai 135, bukan lampiran. Letaknya di akhir dokumen semata-mata karena penomoran tidak boleh disisipi, dan urutan baca yang benar untuk sebuah fitur adalah requirement produknya dahulu, lalu spesifikasi protokolnya.

shared/protocol/schema
Sumber kebenaran tunggal untuk wire protocol. Berisi meta.json, opcodes.json, structs.json, enums.json, dan errors.json.

docs/
Penjelasan teknis turunan dan Architecture Decision Record.

Aturan prioritas bila terjadi perbedaan:

Untuk hal yang menyangkut byte di kabel, schema di shared/protocol/schema menang. Dokumen yang tidak sesuai dengan schema adalah bug dokumen dan harus diperbaiki.

Untuk hal yang menyangkut protocol semantics, section 136 sampai 178 menang atas section produk.

Untuk hal yang menyangkut produk dan fitur, section 1 sampai 135 dan section 179 ke atas menang.

Kata kunci requirement:

WAJIB
Requirement mutlak. Implementasi yang tidak memenuhinya dianggap tidak selesai dan tidak boleh di-merge.

TIDAK BOLEH
Larangan mutlak.

SEBAIKNYA
Sangat dianjurkan. Penyimpangan harus dicatat alasannya di code review atau ADR.

OPSIONAL
Boleh dipilih sesuai kebutuhan.

Penanda status implementasi. Setiap section protocol mencantumkan salah satu penanda berikut supaya dokumen tidak pernah mengklaim sesuatu yang belum ada:

STATUS: BUILT
Sudah diimplementasikan di server dan atau client, memiliki test yang lulus.

STATUS: SCHEMA
Sudah didefinisikan di shared/protocol/schema, code generator sudah menghasilkan Rust dan TypeScript, codec sudah diuji, tetapi handler aplikasinya belum ditulis.

STATUS: SPEC
Baru dispesifikasikan di dokumen ini. Belum masuk ke schema dan belum ada kode.

Requirement dasar yang berlaku untuk seluruh dokumen:

Binary-First Protocol adalah requirement wajib. Seluruh realtime communication client-to-server dan seluruh server-to-server federation WAJIB menggunakan binary protocol Migo Wire Protocol versi 1 yang disingkat MWP/1. Rinciannya ada di section 136 sampai 178.

JSON TIDAK BOLEH digunakan sebagai wire protocol utama untuk realtime path. JSON hanya diperbolehkan pada empat tempat: REST dan public API, configuration file, admin dan debugging tooling, serta test fixture yang dibaca manusia. Daftar lengkap dan batasannya ada di section 118 dan section 137.

Text frame WebSocket TIDAK BOLEH digunakan. Semua frame realtime adalah binary frame.

Polling TIDAK BOLEH digunakan untuk data realtime. Gunakan subscription.

Semua kode program, identifier, komentar, commit message, dan nama file memakai bahasa English. Dokumen ini dan penjelasan di terminal memakai bahasa Indonesia.


1. VISI PRODUK

Migo adalah global real-time communication platform yang menggabungkan:

Private messaging
1-on-1 chat
Group chat
Voice note
Voice call
Video call
Public Room
Managed Room
Global communities
Friend/contact system
User profile
Avatar
Social feed
Virtual gifts
Virtual currency
XP dan level
Badge dan achievement
Mini games
Game bots
Events
Global translation
Moderation
Notification
Multi-device synchronization

Migo harus terasa ringan seperti aplikasi messenger generasi lama, tetapi memiliki kemampuan platform modern.

Prioritas utama:

Low bandwidth
Low latency
High availability
Security
Automatic end-to-end encryption
Binary-first protocol untuk seluruh komunikasi realtime dan federation
Multi-region
Horizontal scalability
Mobile friendly
Web responsive
Native Android
Reliable offline/online synchronization
Efficient server-to-server communication
P2P-first media untuk voice dan video call


2. TARGET PLATFORM

Web:
Next.js App Router
Responsive UI untuk desktop, tablet, Android browser, dan iOS browser
Progressive Web App
Web Crypto API untuk seluruh operasi kriptografi

Android:
Native Android
Kotlin
Jetpack Compose
Minimum Android 9 / API 28
Target Android terbaru yang tersedia saat build
Android Keystore untuk seluruh private key

iOS:
Arsitektur client harus memungkinkan native iOS client dikembangkan kemudian
Keychain dan Secure Enclave bila tersedia

Backend:
Rust
Modular monolith dengan role composition, satu binary bernama migod
Async runtime tokio

Protocol:
Binary-first. Wire protocol realtime adalah MWP/1, sebuah binary framing dengan payload MSE. Lihat section 136 sampai 145.
WebSocket di atas TCP sebagai transport realtime default. Satu MWP frame per satu binary WebSocket message. Text frame TIDAK BOLEH digunakan. permessage-deflate WAJIB dimatikan karena kompresi diputuskan per frame oleh MWP sendiri.
QUIC sebagai transport realtime kedua (opsi) untuk client yang mendukungnya, dinegosiasikan lewat feature bit QUIC. Server hanya mengiklankan bit QUIC bila listener QUIC diaktifkan. Framing di atas QUIC dan TCP memakai length prefix u32 big-endian.
HTTPS di atas HTTP/1.1, HTTP/2, atau HTTP/3 hanya untuk REST dan public API, upload media, admin, dan health endpoint. Bukan untuk chat.
Server-to-server federation memakai TLS 1.3 di atas TCP sebagai transport default, dengan QUIC/TLS 1.3 sebagai opsi kedua bila tersedia, membawa binary federation packet. Lihat section 169.
Transport tanpa enkripsi TIDAK BOLEH ada, termasuk di environment development.

Database:
PostgreSQL sebagai source of truth untuk data transactional dan message. Tabel message dipartisi per bulan.
Redis hanya untuk state ephemeral yang dapat direkonstruksi, seperti presence, typing, session routing, dan rate limit counter. Kehilangan Redis WAJIB tidak menyebabkan kehilangan data.
Pemisahan data transactional, cache, dan message storage mengikuti ADR-0004.

Cache:
Redis, diakses melalui trait sehingga backend in-memory dapat dipakai untuk test dan simulasi deterministik.

Object storage:
S3-compatible storage untuk avatar, image, video, attachment, voice note, dan media lain. Akses selalu melalui signed URL dengan masa berlaku pendek. Bucket TIDAK BOLEH public.


3. ARSITEKTUR MULTI-REGION

Migo menggunakan banyak server yang tersebar di berbagai region.

Contoh:

Asia:
Singapore
Indonesia
Japan
Hong Kong

Europe:
Germany
Netherlands
France
UK

US:
US East
US West

Additional regions dapat ditambahkan kemudian.

Arsitektur:

Client
|
+-- Asia Gateway
+-- Europe Gateway
+-- US Gateway
|
+-- Regional Service Cluster
|
+-- Global P2P/Server Mesh
|
+-- Distributed Storage
|
+-- Global Coordination Layer


4. KONSEP "MULTI P2P SERVER"

Istilah P2P pada Migo digunakan untuk server-to-server mesh.

Server tidak bergantung pada satu central server untuk semua traffic.

Setiap Migo node dapat berkomunikasi dengan node Migo lainnya secara authenticated dan encrypted.

Contoh:

Asia Node A
|
+--- Asia Node B
|
+--- Europe Node A
|       |
|       +--- Europe Node B
|
+--- US Node A
        |
        +--- US Node B

Server dapat:

Discover node
Authenticate node
Exchange routing information
Forward encrypted messages
Synchronize room state
Synchronize presence
Replicate selected metadata
Route users ke region terdekat
Failover ke region lain

Jangan membuat client-to-client P2P sebagai default untuk chat biasa karena dapat mengekspos IP pengguna dan menyulitkan moderation, NAT traversal, reliability, dan E2E security.

Gunakan server-to-server mesh sebagai default.


5. REGION ROUTING

Client saat login memilih gateway terbaik berdasarkan:

Latency
Network quality
Region
Server load
Availability
Connection stability

Contoh:

User Indonesia
-> Singapore Gateway

User Japan
-> Japan Gateway

User Germany
-> Germany Gateway

User USA
-> US East Gateway

Jika gateway gagal:

Primary Gateway
-> Secondary Gateway
-> Another regional gateway


6. SERVER IDENTITY

Setiap server memiliki cryptographic identity.

Setiap node memiliki:

Node ID
Public key
Private key
Region
Country
Capabilities
Software version
Protocol version
Certificate
Health status

Node harus saling melakukan authentication sebelum melakukan server-to-server communication.

Tidak boleh ada server anonim yang dapat masuk ke mesh produksi.


7. SERVER-TO-SERVER SECURITY

STATUS: SPEC untuk implementasi. Desain sudah final di ADR-0005 dan section 169.

Semua komunikasi antar-server WAJIB encrypted dan authenticated. Tidak ada pengecualian untuk development atau staging.

Transport:

TLS 1.3 di atas TCP sebagai transport default
QUIC/TLS 1.3 sebagai opsi kedua untuk deployment yang mendukung UDP
Cipher suite modern saja, tanpa downgrade path

Payload:

Seluruh federation packet adalah binary MWP/1 dengan opcode pada range federation. JSON TIDAK BOLEH dipakai untuk federation.

Server identity:

Setiap node memiliki satu Ed25519 keypair sebagai cryptographic node identity
Node dikenali dari public key, bukan dari hostname atau IP
Allow-list berbasis public key. Node yang tidak ada di allow-list ditolak pada handshake, sebelum sesi terbentuk

Mutual authentication:

Kedua sisi WAJIB membuktikan kepemilikan private key. TLS client certificate saja tidak cukup, karena identitas mesh Migo hidup di layer aplikasi agar tidak bergantung pada PKI eksternal.

Signature WAJIB mengikat seluruh konteks berikut sekaligus:

Domain separation string "migo-mesh-v1"
Protocol version
Signer node id
Peer node id
Nonce pengirim
Nonce penerima
Timestamp

Mengikat semuanya sekaligus membuat signature tidak dapat dipakai ulang untuk link lain, arah lain, atau versi protocol lain.

Replay protection:

Nonce acak 32 byte dari kedua sisi pada setiap handshake
Toleransi clock skew maksimum 60 detik ke dua arah
Nonce yang sudah dipakai ditolak selama jendela toleransi masih berlaku
Setiap link memiliki sequence number monotonic. Packet dengan sequence yang tidak lebih besar dari sequence terakhir ditolak dan dicatat

Perlindungan operasional:

Message authentication pada setiap packet
Rate limiting per peer
Connection limit per peer
Node allow-list dan deny-list
Protocol version validation, node dengan versi tak dikenal ditolak dengan error code, bukan dengan crash
Health check periodik, lihat section 170
Key rotation dengan masa tumpang tindih, lihat section 169

Private key node TIDAK BOLEH disimpan di dalam source code, di dalam Git, atau di dalam image container. Private key diambil dari secret manager atau environment variable yang disuntikkan saat runtime. migod WAJIB menolak start pada environment production bila secret masih kosong atau masih memakai nilai default development.

Istilah P2P pada Migo hanya berlaku untuk server mesh dan untuk media WebRTC pada voice/video call. Chat teks biasa TIDAK BOLEH dikirim langsung client-to-client, karena itu akan membocorkan alamat IP kedua pihak dan menghilangkan moderation, rate limiting, serta reliable delivery.


8. AUTOMATIC END-TO-END ENCRYPTION

Migo menggunakan automatic E2E encryption. User tidak perlu mengaktifkan encryption secara manual dan tidak dapat mematikannya untuk komunikasi private.

Prinsip yang tidak dapat dinegosiasikan:

Enkripsi WAJIB dilakukan di client, sebelum payload masuk ke server. Server menerima ciphertext yang sudah tersegel, bukan plaintext yang lalu dienkripsi oleh server.
Server TIDAK BOLEH memiliki kemampuan membaca plaintext private message, untuk peran apa pun, termasuk administrator, operator, dan support.
TIDAK BOLEH ada key escrow, master key, atau recovery key milik server.
TIDAK BOLEH membuat primitive kriptografi sendiri. Hanya library yang sudah diaudit.

Cakupan E2E:

Private chat 1-on-1
E2E encrypted by default.

Group chat
E2E encrypted by default menggunakan sender-key ratchet.

Voice note pada private chat dan group chat
E2E encrypted. Audio dikompresi lalu dienkripsi di client sebelum upload. Lihat section 167.

Call signaling untuk private call
Payload sensitif pada signaling, yaitu SDP dan ICE candidate, dienkripsi end-to-end antar device. Lihat section 165.

Voice call dan video call 1-on-1
P2P-first dengan E2E. Media tidak melewati server bila P2P berhasil. Lihat section 166.

Group call
SFU dengan E2E. SFU hanya meneruskan paket dan tidak memiliki akses ke plaintext media.

Public Room dan Managed Room
TIDAK E2E. Pesan room dienkripsi pada transport dan dapat dibaca server. Ini keputusan sadar, karena Public Room dan Managed Room membutuhkan moderation, filter, search, dan bot yang bekerja di server. Room tidak boleh menjanjikan sesuatu yang tidak dapat dipenuhi.

Status encryption per conversation direpresentasikan oleh enum EncryptionMode dengan nilai None, Transport, atau EndToEnd, sehingga client tidak perlu menyimpulkannya dari jenis conversation. Status itu WAJIB ditampilkan dengan jujur dan tanpa ambiguitas. Teks yang dipakai:

Private Chat
"End-to-end encrypted"

Group Chat
"End-to-end encrypted"

Public Room
"Encrypted transport, dapat dibaca server untuk moderation"

Managed Room
"Encrypted transport, dapat dibaca server untuk moderation"

Voice Call dan Video Call private
"End-to-end encrypted"

Group Call
"End-to-end encrypted"

UI TIDAK BOLEH memakai kata end-to-end untuk Public Room atau Managed Room. Klaim keamanan yang tidak akurat lebih berbahaya daripada tidak ada klaim sama sekali.

Public Room dan Managed Room dengan E2E berada di luar lingkup MWP/1. Requirement produk aslinya menyebutnya sebagai kemungkinan opsional, dan itu tetap terbuka sebagai desain terpisah, tetapi bukan bagian dari versi protocol ini. Alasannya bukan kesulitan teknis enkripsi, melainkan bahwa moderation, search di server, filter, dan bot yang membaca isi room adalah janji produk yang sudah dibuat di section 19, section 20, dan section 49. Dua janji itu tidak dapat dipenuhi bersamaan tanpa memilih salah satunya, dan pilihan itu harus eksplisit, bukan implisit.


9. E2E KEY MANAGEMENT

Ringkasan di sini. Spesifikasi protocol lengkap ada di section 163 dan section 164, keputusan desain ada di ADR-0003.

Setiap account memiliki identity key. Setiap device memiliki key material sendiri, karena satu account dapat memiliki banyak device dan satu device yang hilang tidak boleh membocorkan device lain.

Identity key terdiri dari dua key, bukan satu:

Ed25519 untuk signing
X25519 untuk key exchange

Keduanya digabung menjadi identity blob 64 byte. Dua key dipilih daripada satu key dengan konversi, supaya tidak ada operasi kriptografi non-standar di jalur kritis.

Primitive yang dipakai. Semuanya dari library yang telah diaudit:

X3DH untuk pembentukan session 1-on-1
Double Ratchet untuk forward secrecy dan post-compromise security pada chat 1-on-1
Sender-key ratchet untuk group
HKDF-SHA256 dengan label berbeda untuk setiap tujuan derivasi
XChaCha20-Poly1305 sebagai AEAD
Argon2id untuk password hashing dengan parameter 19 MiB memory, 2 pass, 1 lane
Ed25519 untuk signature
HMAC-SHA256 untuk opaque token, bukan JWT
Perbandingan bernilai rahasia WAJIB constant-time

Jangan membuat algoritma kriptografi sendiri. Jangan menyusun mode enkripsi sendiri di atas primitive rendah.

Private keys:

Dibuat di device, tidak pernah dibuat di server
Disimpan terenkripsi atau non-extractable di device
TIDAK PERNAH dikirim ke server, dalam bentuk plaintext maupun terenkripsi dengan key milik server

Server hanya menerima:

Public identity key
Signed prekey beserta signature dan waktu kedaluwarsanya
Sekumpulan one-time prekey
Encrypted payload

Server menyerahkan satu key bundle ketika pengirim memintanya, dan satu one-time prekey dikonsumsi pada setiap pengambilan. Ketika one-time prekey habis, session tetap dapat dibentuk dengan signed prekey saja, dengan properti forward secrecy yang lebih lemah pada pesan pertama. Client WAJIB mengisi ulang one-time prekey sebelum habis.

Private key storage:

Android:
Android Keystore. Key non-exportable. Operasi kriptografi dilakukan melalui Keystore bila key type mendukungnya.

Web:
Web Crypto API dengan CryptoKey non-extractable. Key material disimpan di IndexedDB sebagai CryptoKey object, bukan sebagai byte array.
Private key TIDAK BOLEH disimpan di localStorage, sessionStorage, cookie, atau di dalam URL, baik plaintext maupun hasil encoding.
WebAuthn atau passkey OPSIONAL untuk melindungi unlock.

iOS:
Keychain dan Secure Enclave bila tersedia.

Key change WAJIB terlihat oleh user. Ketika identity key peer berubah, client menampilkan peringatan dan menyediakan verifikasi safety number. Perubahan key yang senyap adalah cara serangan man-in-the-middle bekerja.


10. PRIVATE MESSAGE FLOW

User A mengirim pesan kepada User B.

Flow:

User A
|
Encode plaintext, lalu encrypt locally
|
Sealed envelope berupa byte, bukan JSON
|
MWP frame MESSAGE_SEND, opcode 32
|
Nearest Migo Gateway
|
Encrypted routing antar region bila perlu, opcode FED_FORWARD
|
User B Gateway
|
MWP frame MESSAGE_EVENT, opcode 33
|
User B
|
Decrypt locally

Server TIDAK BOLEH membaca plaintext private message. Server tidak menyimpan key untuk membukanya, sehingga ini adalah sifat arsitektur, bukan sekadar kebijakan.

Yang diketahui server pada jalur ini hanya metadata minimum yang memang diperlukan untuk routing dan billing byte:

message_id yang dibuat client
conversation_id
kind, yaitu jenis pesan seperti Text, Media, atau Voice
sender_id dan sender_device
sender_key_id
seq yang diberikan server
created_at
panjang envelope
reply_to bila ada
expires_in_ms bila pesan bersifat sementara

Isi teks, isi media, nama file, durasi voice note yang bersifat sensitif, dan seluruh detail lain berada di dalam envelope dan tidak dapat dibaca server.

Acknowledgement:

Server membalas dengan response struct MessageAccepted pada correlation MESSAGE_SEND, berisi seq dan created_at
Bila message_id sudah pernah diterima, server membalas MessageAccepted dengan duplicate bernilai true dan seq yang sama seperti pengiriman pertama, tanpa membuat pesan baru
Penerima mengirim MESSAGE_RECEIPT, opcode 34, sebagai watermark kumulatif, bukan satu receipt per pesan


11. E2E MESSAGE FORMAT

Ada dua envelope yang berbeda dan keduanya binary. Membedakan keduanya penting, karena satu diperiksa server dan satu tidak dapat diperiksa server.

Envelope pertama, wire envelope. Ini adalah MWP/1 frame yang dibaca server. Spesifikasi lengkap ada di section 139.

Envelope kedua, cryptographic envelope. Ini adalah byte hasil enkripsi client yang diletakkan di field envelope pada MessageSend dan MessageEvent. Server hanya melihatnya sebagai byte dengan panjang tertentu.

Layout cryptographic envelope untuk chat 1-on-1. STATUS: SPEC.

Semua field ditulis biner tanpa nama field. Urutan tetap, tidak ada JSON, tidak ada nama field di kabel:

envelope_version, u8
scheme, u8, nilainya menyatakan Double Ratchet 1-on-1 atau Sender Key untuk group
sender_key_id, varint
ratchet_public_key, 32 byte, hanya ada bila scheme memerlukan
message_counter, varint
previous_chain_length, varint
ciphertext, byte sampai akhir minus 16 byte
authentication_tag, 16 byte

Associated data untuk AEAD WAJIB mengikat metadata yang tidak dienkripsi, minimal:

envelope_version
scheme
message_id
conversation_id
sender_id
sender_device
sender_key_id

Mengikat metadata pada tag berarti server tidak dapat memindahkan ciphertext ke conversation lain atau mengganti identitas pengirim tanpa merusak verifikasi. Tanpa pengikatan ini, E2E melindungi isi tetapi tidak melindungi konteks.

Plaintext di dalam ciphertext juga binary dan compact. Layout plaintext:

content_type, u8, misalnya Text, MediaRef, VoiceNoteRef, Reaction, atau ControlEvent
body, byte, berisi struct MSE sesuai content_type
Padding OPSIONAL, dengan bucket panjang tetap, untuk mengurangi kebocoran panjang pesan

JSON TIDAK BOLEH dipakai di dalam cryptographic envelope. Menggunakan JSON di dalam ciphertext membuang byte pada setiap pesan dan tetap membocorkan struktur melalui panjang.

Group message memakai layout yang sama dengan scheme Sender Key, ditambah:

group_key_epoch, varint, naik setiap kali keanggotaan berubah sehingga member yang keluar tidak dapat membaca pesan berikutnya


12. BANDWIDTH OPTIMIZATION

Migo dirancang dengan prinsip:

"Do more with less bytes."

Target angka per event ada di section 56 dan section 171. Yang di bawah ini adalah mekanismenya, dan semuanya bersifat WAJIB.

Serialization:

Binary protocol MWP/1 dengan payload MSE untuk seluruh realtime path
JSON TIDAK BOLEH dipakai pada realtime path
MessagePack dan CBOR TIDAK BOLEH dipakai, karena keduanya self-describing sehingga nama field ikut terkirim pada setiap pesan
Protobuf tidak dipakai. Alasannya ada di section 143 dan ADR-0002. Ringkasnya, MSE membayar overhead tag hanya pada optional field, sedangkan required field tidak membayar apa pun

Compact identifier:

Semua id adalah 16 byte biner, bukan string UUID 36 karakter. Selisihnya 20 byte per id, dan satu MessageEvent membawa empat id
Timestamp adalah varint milidetik terhitung dari Migo epoch 2024-01-01T00:00:00Z, bukan string ISO-8601. Selisihnya sekitar 19 byte per timestamp
Enum dikirim sebagai varint, bukan sebagai string

Delta updates:

Room state, member list, game state, leaderboard, dan counter dikirim sebagai perubahan, bukan snapshot
Snapshot penuh hanya pada join atau pada resync yang gagal

Batching:

Frame kecil digabung menjadi satu WebSocket message dengan linger maksimum 15 ms
Batasan MAX_BATCH_ITEMS adalah 256 item per frame
Satu radio wake-up jauh lebih mahal daripada beberapa ratus byte tambahan, sehingga satu frame 2 KB lebih baik daripada dua puluh frame 100 byte

Message coalescing:

Event berkelas Coalescable, yaitu presence, typing, dan counter, hanya menyisakan nilai terbaru per coalescing key di dalam satu linger window

Pagination dan cursor:

Semua listing memiliki batas maksimum server-side
History diambil dengan cursor seq, bukan dengan offset

Sequence number dan ACK:

Setiap conversation memiliki seq monotonic sehingga client dapat mendeteksi gap
ACK memakai watermark kumulatif, satu ACK melunasi ratusan frame

Deduplication:

message_id yang dibuat client menjadi idempotency key, sehingga retry tidak menghasilkan pesan ganda

Compression:

Payload dikompresi hanya bila ketiga syarat terpenuhi: fitur compression dinegosiasikan, ukuran payload minimal 512 byte, dan hasil kompresi minimal 10 persen lebih kecil
Payload kecil TIDAK BOLEH dikompresi, karena overhead header kompresi dapat lebih besar daripada datanya dan biaya CPU terasa pada baterai

Connection reuse:

Satu WebSocket atau QUIC connection per instance aplikasi untuk semua fitur realtime. TIDAK BOLEH membuka koneksi terpisah per fitur
HTTP/2 atau HTTP/3 dengan connection reuse untuk REST

Media:

Media tidak melewati chat server. Upload dan download langsung ke object storage memakai signed URL
Thumbnail lebih dulu, progressive loading, adaptive quality, lazy loading
Voice note dikompresi di client dengan codec speech, bukan format lossless

Offline dan sinkronisasi:

Offline queue untuk pesan, media, reaction, dan friend action
Incremental sync dengan have_seq, bukan full resync
Local cache, ETag dan conditional request untuk asset statis

Adaptive:

Presence dan typing dikurangi frekuensinya sesuai bandwidth mode
Bandwidth mode dikirim pada HELLO sehingga server berhenti mengirim yang tidak akan dirender client. Filter di client menghemat rendering, filter di server menghemat byte

Aturan penutup yang paling sering dilanggar:

TIDAK BOLEH mengirim data yang tidak berubah. Bila tidak ada perubahan, tidak ada frame.
TIDAK BOLEH melakukan polling. Bila muncul kebutuhan setInterval dan fetch, jawabannya adalah subscription.


13. CHAT BANDWIDTH OPTIMIZATION

Jangan kirim seluruh conversation setiap membuka chat.

Mekanisme yang dipakai, dengan opcode sebenarnya:

Resume session lebih dahulu bila session lama masih dalam resume window. Frame yang tertahan diputar ulang tanpa query database
CONVERSATION_LIST untuk daftar conversation beserta last_seq masing-masing
SYNC hanya untuk conversation yang benar-benar punya gap, dengan have_seq berisi seq contiguous tertinggi yang dimiliki client
Cursor untuk paging history lama, bukan offset
Incremental sync, bukan full resync

Contoh:

Client terakhir menerima:
seq 1000

Server:
SyncResponse status Ok dari enum SyncStatus, from_seq 1001, to_seq 1012, more false

Server hanya mengirim 12 message.

Bukan seluruh history.

Conversation yang tidak berubah TIDAK BOLEH menghasilkan traffic sama sekali. Detail urutan sinkronisasi ada di section 158.


14. PRESENCE OPTIMIZATION

Jangan broadcast online status setiap beberapa detik.

Mekanisme:

Presence heartbeat mengikuti interval yang ditentukan server pada Welcome
Hanya kirim event ketika state berubah, memakai opcode PRESENCE_SET dan PRESENCE_EVENT
PRESENCE_EVENT berkelas Coalescable, sehingga event lama untuk user yang sama digantikan oleh yang terbaru ketika queue menumpuk
Presence berbasis scope. Client hanya menerima presence untuk conversation dan room yang di-subscribe
Untuk room besar, presence dikirim sebagai aggregate count, bukan daftar per anggota
Antar region, presence dikirim sebagai digest teragregasi lewat FED_PRESENCE_DIGEST, bukan event per user

State yang tersedia berasal dari enum PresenceState:

Offline
Online
Away
Busy
Invisible

Invisible ditegakkan di server. Client tidak boleh dipercaya untuk menyembunyikan presence dirinya sendiri.

Jika tidak ada perubahan status, jangan broadcast ulang. Interval adaptif per bandwidth mode ada di section 159.


15. TYPING INDICATOR

Typing event harus sangat ringan.

Jangan mengirim setiap keypress.

Bentuk sebenarnya di protocol adalah satu opcode TYPING dengan payload TypingEvent yang memuat conversation_id dan state dari enum TypingState:

Start
Stop

Aturan:

Client melakukan debounce. Start dikirim sekali lalu di-refresh paling cepat setiap beberapa detik selama user masih mengetik
Stop dikirim ketika user berhenti, mengirim pesan, atau meninggalkan conversation
Penerima menerapkan timeout lokal, sehingga Start yang tidak pernah diikuti Stop tetap hilang sendiri
TYPING berkelas Coalescable dengan key conversation_id dan user_id, sehingga hanya state terakhir yang dikirim ketika queue menumpuk
Typing event TIDAK BOLEH masuk offline queue. Indikator typing yang terkirim terlambat adalah informasi yang salah, bukan informasi yang tertunda
Pada mode UltraLowData, typing dimatikan sepenuhnya

Overhead satu typing event ditargetkan tidak lebih dari 12 byte. Lihat section 56 dan section 159.


16. MEDIA OPTIMIZATION

Image:

Generate thumbnail
WebP/AVIF
Adaptive resolution
Lazy loading
Progressive loading

Video:

Thumbnail dahulu
Adaptive bitrate
Resolution berdasarkan device dan network
Streaming
Optional download

Avatar:

Small optimized image
Aggressive cache

Sticker:

Compressed assets
CDN dan cache

Voice note:

Codec speech dengan bitrate rendah, bukan format lossless
Dikompresi dan dienkripsi di client sebelum upload
Chunked upload yang dapat di-resume
Waveform dikirim sebagai data compact, lihat section 167

File besar:

Upload langsung ke object storage menggunakan signed URL
Chunked upload dengan resume, sehingga kegagalan pada 80 persen dilanjutkan dari sekitar 80 persen dan bukan dari nol

Aturan wajib:

Jangan proxy seluruh file besar melalui chat server. Chat server hanya menerbitkan ticket dan menyimpan reference.
Bucket object storage TIDAK BOLEH public. Akses hanya melalui signed URL dengan masa berlaku pendek.
Media pada private chat WAJIB dienkripsi di client sebelum upload. Object storage menyimpan ciphertext.
Batas MAX_FRAME_BYTES adalah 262144 byte. Apa pun yang lebih besar dari itu masuk ke object storage, bukan ke frame.


17. OFFLINE-FIRST CLIENT

Client harus tetap dapat digunakan ketika koneksi buruk atau tidak ada.

Saat offline:

Buka cached chats
Baca message history
Tulis pesan
Queue message
Queue media upload
Queue voice note
Queue reactions
Queue friend actions
Queue read receipt sebagai watermark tunggal, bukan satu per pesan

Yang TIDAK BOLEH di-queue:

Call invite dan seluruh call signaling. Call bersifat realtime dan kehilangan maknanya bila dikirim terlambat. Bila offline, tombol call dinonaktifkan dengan alasan yang jelas.
Typing indicator. Typing yang tertunda adalah informasi salah.

Outbox:

Setiap item queue memiliki message_id yang dibuat client sebelum pengiriman, sehingga retry bersifat idempotent
Item queue disimpan durable di device, bukan hanya di memori, supaya restart aplikasi tidak menghilangkannya
Urutan pengiriman per conversation dipertahankan

Saat online:

Reconnect
Authenticate
Resume bila memungkinkan, lihat section 150
Incremental sync per conversation dengan have_seq, lihat section 158
Upload queued messages dan media
Receive missing messages hanya pada range yang hilang
Resolve conflicts

Aturan konflik:

Pesan tidak pernah berkonflik, karena server yang memberi seq dan seq tidak pernah berubah
Untuk state yang dapat diubah dua pihak, seperti profile atau room setting, versi server menang dan perubahan lokal yang gagal ditampilkan kembali kepada user, tidak dibuang diam-diam
Read cursor bergerak maju saja. Nilai yang lebih kecil dari nilai tersimpan diabaikan


18. CONNECTION MANAGEMENT

Client menggunakan satu connection manager untuk seluruh fitur realtime. Satu socket per instance aplikasi. TIDAK BOLEH ada socket terpisah per fitur.

Status:

Disconnected
Connecting
Connected
Reconnecting
Offline
Suspended

Handshake dan heartbeat:

HELLO lalu Welcome. Server yang menentukan interval heartbeat pada Welcome, dengan DEFAULT_HEARTBEAT_MS bernilai 30000 ms
Client mengirim PING pada cadence tersebut dan memperpanjangnya pada battery saver atau low-data mode
Melewatkan dua interval berarti socket ditutup dan masuk ke Reconnecting

Reconnect menggunakan exponential backoff dengan full jitter.

Basis delay:

1s
2s
4s
8s
16s
30s

Delay yang dipakai adalah nilai acak di antara nol dan basis delay tersebut. Full jitter dipilih supaya lima puluh ribu client tidak kembali pada detik yang sama setelah satu node jatuh.

Jangan reconnect terus-menerus tanpa batas. Setelah mencapai 30s, delay bertahan di 30s dengan jitter, dan attempt counter tetap dicatat untuk observability.

Aturan tambahan:

Bila server mengirim RECONNECT_HINT, client WAJIB menunggu after_ms yang diberikan dan memakai endpoint yang disarankan bila ada. Ini yang dipakai saat drain dan rebalance
Bila reason adalah AuthExpired, client refresh token lebih dulu, tidak sekadar reconnect
Bila reason adalah ProtocolViolation, client TIDAK BOLEH reconnect otomatis. Itu bug client dan harus terlihat
Saat aplikasi masuk background di Android, socket ditutup rapi dan notifikasi dilanjutkan lewat push. Menahan socket di background menghabiskan baterai tanpa manfaat
Reconnect saat jaringan berpindah, misalnya WiFi ke seluler, dimulai segera tanpa menunggu backoff, karena penyebabnya sudah diketahui dan bukan karena server bermasalah


19. PUBLIC ROOM

Public Room adalah ruang chat yang dapat ditemukan dan diikuti oleh pengguna.

Contoh:

Indonesia
Jakarta
Gaming
Music
Technology
Crypto
Anime
Random
Language rooms
Country rooms
City rooms

Public Room memiliki:

Room ID
Name
Description
Category
Tags
Language
Country
Avatar
Banner
Member count
Online count
Topic
Rules
Owner
Moderators
Status


20. MANAGED ROOM

Managed Room adalah Public Room atau Community Room yang memiliki pengelolaan khusus.

Struktur:

Owner
Manager
Administrator
Moderator
Helper
Member

Managed Room dapat memiliki:

Custom rules
Custom permissions
Member approval
Invite system
Ban
Mute
Kick
Warning
Slow mode
Anti-spam
Anti-flood
Word filtering
Link filtering
Media restrictions
Pinned announcements
Scheduled announcements
Welcome message
Audit logs
Moderator logs
Report management
Custom branding
Custom room theme


21. PUBLIC ROOM VS MANAGED ROOM

Public Room:

Terbuka
Simple moderation
User dapat join langsung
Community-driven
Discovery-focused

Managed Room:

Dikelola owner/manager
Moderation lebih kuat
Role system
Permission system
Member approval
Custom rules
Audit logs
Advanced anti-spam
Custom branding

Satu Managed Room tetap dapat dibuat discoverable/public.


22. GROUP CHAT

Group Chat berbeda dengan Room.

Group Chat lebih berorientasi pada kelompok tertentu.

Fitur:

Private group
Public group
Invite link
Admin
Moderator
Member roles
Message history
Pinned messages
Polls
Media
Files
Voice note
Group voice call
Group video call
Group settings

Group chat bersifat E2E encrypted memakai sender-key ratchet dengan group_key_epoch. Perubahan keanggotaan memicu re-keying, sehingga anggota yang keluar tidak dapat membaca pesan berikutnya. Group call memakai SFU dengan E2E. Lihat section 9, section 166, dan section 167.


23. PRIVATE CHAT

Private chat:

1-on-1
E2E encrypted
Message reactions
Reply
Forward
Edit
Delete
Voice note
Photo
Video
File
Sticker
GIF
Voice call P2P dengan E2E
Video call P2P dengan E2E
Disappearing messages
Pinned messages
Search
Mute
Archive

Semua isi private chat, termasuk voice note dan payload sensitif pada call signaling, dienkripsi di client sebelum masuk ke server. Voice note memakai MESSAGE_SEND dengan kind bernilai Voice dari enum MessageKind, sehingga ikut memakai ordering, dedup, offline queue, receipt, dan sync yang sama seperti pesan teks. Lihat section 11, section 165, dan section 167.


24. FRIEND SYSTEM

User dapat:

Add friend
Accept
Reject
Remove
Block
Follow
Unfollow
Favorite

Discover:

Online users
Suggested users
Mutual friends
Same interests
Same country
Same rooms


25. USER PROFILE

Profile:

Username
Display name
Unique user ID
Avatar
Cover
Bio
Country
Language
Timezone
Interests
Status
Level
XP
Badges
Achievements
Friends
Followers
Following
Gift collection
Joined date


26. STATUS SYSTEM

User dapat memiliki:

Online
Away
Busy
Invisible
Offline

Custom status:

"Playing games"
"Working"
"Listening to music"
"Do not disturb"

Status harus configurable berdasarkan privacy settings.


27. AVATAR SYSTEM

Avatar:

2D avatar
3D avatar bila diperlukan
Hair
Face
Clothing
Accessories
Hat
Glasses
Shoes
Avatar frame
Profile frame
Animated cosmetics


28. VIRTUAL GIFTS

User dapat mengirim:

Rose
Heart
Cake
Star
Diamond
Crown
Rocket
Dragon
Fire
Mystery gift

Gift dapat:

Animated
Limited
Seasonal
Rare
Collectible

Gift dapat muncul dalam profile dan chat.


29. VIRTUAL CURRENCY

Contoh:

Coins
Gems
Points

Penggunaan:

Buy gifts
Buy avatar items
Buy stickers
Buy profile frame
Buy themes
Buy cosmetics
Participate in events

Economy harus memiliki anti-abuse system.

Jangan memberikan currency berdasarkan message count secara naif karena dapat dieksploitasi spam bots.


30. XP / LEVEL

XP diperoleh dari aktivitas berkualitas:

Daily activity
Achievements
Events
Games
Community contribution
Helpful actions
Room participation

Gunakan anti-farming system.


31. BADGES

Contoh:

Early User
Veteran
Top Chatter
Room Creator
Manager
Moderator
Community Leader
Helpful
Gift Master
Game Champion
Global Explorer
Verified
Event Winner


32. LEADERBOARD

Global leaderboard
Country leaderboard
Room leaderboard
Game leaderboard
Weekly
Monthly
All-time

Kategori:

XP
Level
Games
Achievements
Gifts
Community reputation


33. SOCIAL FEED

Migo dapat memiliki social feed:

Text
Image
Video
Poll
GIF
Like
Comment
Share
Repost
Mention
Hashtag

Feed tidak boleh mengganggu fungsi utama messenger.


34. GLOBAL CHAT DISCOVERY

Discover:

People
Public Rooms
Managed Rooms
Groups
Events
Games
Trending communities

Filter:

Country
Language
Category
Popularity
Online
New
Trending


35. REAL-TIME TRANSLATION

Optional automatic translation.

Contoh:

User Indonesia:
"Halo semuanya"

User Jepang melihat:
"みなさん、こんにちは"

User dapat memilih:

Original
Translated
Both

Translation dilakukan hanya ketika diperlukan agar hemat bandwidth.


36. GAME BOTS

Migo harus memiliki sistem Bot Framework.

Bot bukan hanya chatbot, tetapi dapat menjadi anggota Room dan menjalankan mini games.

Bot memiliki:

Bot ID
Bot username
Bot avatar
Bot permissions
Bot commands
Bot owner
Bot status
Bot rate limit


37. GAME BOTS DI ROOM

Contoh:

/help
/games
/8ball
/dice
/quiz
/trivia
/word
/coin
/guess
/rps
/blackjack-free
/slots-free

Semua game harus menggunakan virtual points atau reward non-monetary jika diperlukan.

Tidak boleh menggunakan real-money gambling mechanics.


38. MINI GAME SYSTEM

Game dapat dimainkan langsung di chat.

Contoh:

Snake
Tetris
2048
Tic Tac Toe
Rock Paper Scissors
Trivia
Quiz
Word Game
Guess Number
Chess
Checkers
Memory Game

Game dapat mendukung:

Single player
Room multiplayer
Turn-based
Real-time
Leaderboard
XP
Achievements
Cosmetic rewards


39. GAME BOT ARCHITECTURE

Room
|
Game Bot
|
Game Engine
|
Game State
|
Players

Game state harus compact.

Jangan broadcast seluruh game state setiap tick.

Gunakan event/delta:

player_joined
move
attack
score_changed
round_finished

Server hanya mengirim perubahan.


40. BOT BANDWIDTH OPTIMIZATION

Bot tidak boleh spam chat.

Gunakan:

Command response
Event batching
Cooldown
Rate limiting
Compact payload
State delta
Cached leaderboard

Contoh:

User:

/dice

Bot:

@user rolled 6


Bukan mengirim banyak status update.


41. BOT SDK

Developer dapat membuat bot menggunakan:

Rust SDK
Web API
Bot API
Webhook
WebSocket

Bot permission:

Read messages
Send messages
Moderate
Manage games
Read member list
Send announcements

Default bot harus memiliki permission minimum.


42. BOT SECURITY

Bot harus berjalan dalam sandbox atau isolated execution environment bila memungkinkan.

Batasi:

CPU
Memory
Network
Requests
Message rate
Room access
Permissions

Bot tidak boleh memiliki akses database langsung.


43. EVENT SYSTEM

Migo mendukung:

Daily events
Weekly events
Monthly events
Seasonal events
Room events
Game tournaments
Community competitions

Reward:

XP
Coins
Gems
Badges
Titles
Avatar items
Frames


44. NOTIFICATION SYSTEM

Notification:

Message
Voice note
Missed call
Incoming call
Friend request
Mention
Reply
Gift
Level up
Achievement
Room invite
Room announcement
Event
Game challenge

Gunakan push notification dan local notification.

Jangan mengirim push notification untuk setiap event kecil.

Push untuk komunikasi private hanya berfungsi sebagai wake-up. Payload push TIDAK BOLEH memuat plaintext pesan, plaintext audio voice note, atau isi signaling. Push untuk incoming call hanya memuat call_id dan penanda bahwa ada call, lalu client menarik detailnya melalui koneksi realtime. Lihat section 77 dan section 165.


45. SECURITY MODEL

Security harus menjadi bagian dari architecture, bukan fitur tambahan.

Gunakan:

TLS 1.3
E2E encryption
Secure key storage
Cryptographic identity
Session authentication
Device authentication
Token rotation
Refresh token rotation
Rate limiting
Replay protection
Input validation
Output encoding
Permission checks
Audit logs
Abuse detection
Anti-spam
Anti-flood
DDoS protection


46. ACCOUNT SECURITY

Support:

Email
Username
Password
Passkey
2FA
Recovery codes
Device sessions
Remote logout
PIN/app lock

Password tidak pernah disimpan plaintext.

Gunakan password hashing modern dengan parameter yang aman.


47. DEVICE SECURITY

Setiap device memiliki identity kriptografis sendiri, yaitu satu Ed25519 signing key dan satu X25519 key agreement key yang dibuat di device dan tidak pernah dikirim ke server.

Contoh:

Android Phone
Chrome Desktop
iOS

User dapat melihat:

Device name
Last active
Region
Session time
Safety number untuk verifikasi identity

User dapat revoke device.

Device yang dicabut kehilangan kemampuan mengambil key bundle baru, session ratchet-nya dihentikan, dan key call-nya tidak lagi dapat dipakai. Perubahan identity key WAJIB ditampilkan sebagai peringatan yang terlihat, bukan diterima diam-diam. Penyimpanan key per platform diatur di section 164.


48. PERMISSION SYSTEM

Permission harus granular.

Nama di bawah ini adalah permission produk di level room dan conversation, bukan opcode. Opcode ada di section 145, dan satu opcode dapat memerlukan lebih dari satu permission.

Contoh:

CHAT_SEND
CHAT_DELETE
CHAT_PIN
VOICE_NOTE_SEND
VOICE_NOTE_DELETE
VOICE_NOTE_FORWARD
VOICE_NOTE_PLAY
CALL_START
CALL_JOIN
USER_MUTE
USER_KICK
USER_BAN
ROOM_EDIT
ROOM_MANAGE
ROOM_INVITE
ROOM_ANNOUNCE
ROOM_MODERATE
BOT_USE
BOT_MANAGE

Permission diperiksa di server. Client boleh menyembunyikan tombol untuk pengalaman yang lebih baik, tetapi UI yang menyembunyikan tombol bukan enforcement. Permintaan yang tidak diizinkan dijawab PERMISSION_DENIED, dan untuk objek yang seharusnya tidak diketahui keberadaannya dijawab NOT_FOUND agar tidak membocorkan eksistensi. Lihat section 161.


49. MODERATION SYSTEM

Global moderation:

User report
Message report
Room report
Bot report

Automated detection:

Spam
Flood
Scam
Malicious links
Abusive behavior
Bot abuse

Moderator dashboard:

Reports
Users
Rooms
Messages
Bans
Warnings
Audit logs


50. ANTI-SPAM

Rate limit:

Message per second
Message per minute
Room join rate
Friend request rate
Gift rate
Bot command rate
API request rate

Gunakan adaptive rate limiting.


51. DATABASE ARCHITECTURE

Pisahkan data:

Identity
Users
Devices
Sessions
Conversations
Messages
Rooms
Room members
Permissions
Presence
Friends
Gifts
Currency
XP
Achievements
Games
Bots
Events
Moderation
Audit logs

Jangan menyimpan semua data dalam satu tabel besar.


52. MESSAGE STORAGE

Private E2E messages disimpan sebagai encrypted ciphertext.

Server tidak membutuhkan plaintext.

Message lifecycle:

Created
Encrypted
Queued
Delivered
Acknowledged
Read
Expired/deleted


53. GLOBAL MESSAGE ROUTING

User A Asia
User B Europe

Flow:

User A
-> Asia Gateway
-> Encrypted server mesh
-> Europe Gateway
-> User B

Payload tetap encrypted.

Gateway tidak melakukan decryption terhadap private E2E content.


54. ROOM ROUTING

Public/Managed Room dapat memiliki home region.

Contoh:

Room ID:
migo://room/abc123

Home region:
Asia

User Europe:

Europe Gateway
-> Asia Room Node
-> Europe Gateway
-> Client

Jika room sangat besar, gunakan regional room edge/cache.

Setiap room memiliki tepat satu sequencer di home region-nya. Sequencer itulah yang memberi nomor seq pada pesan room, sehingga urutan pesan tunggal dan tidak perlu direkonsiliasi antar region. Region lain hanya meneruskan dan menyebarkan.

Perubahan home region atau perpindahan shard menaikkan routing epoch. Node yang masih memakai epoch lama menerima ROUTING_EPOCH_STALE lalu menyegarkan peta routing-nya. Ketika region home tidak terjangkau, room menjadi read-only dan client menerima ROOM_READ_ONLY_PARTITION. Memilih sequencer kedua TIDAK BOLEH dilakukan, karena dua sequencer berarti dua urutan yang tidak dapat digabungkan lagi. Detail lengkap ada di section 170.


55. LARGE ROOM SCALING

Untuk room dengan ribuan atau jutaan users:

Jangan membuat setiap message menjadi individual server broadcast operation.

Gunakan:

Room shards
Fanout service
Regional relays
Batch delivery
Presence aggregation
Message sequence
Subscription groups

Contoh:

Room
|
+-- Asia shard
+-- Europe shard
+-- US shard

Message diterima satu kali oleh regional shard lalu didistribusikan ke subscribers.


56. BANDWIDTH TARGET

Target ini adalah budget, bukan harapan. Perubahan yang melewatinya WAJIB disertai alasan pada code review. Angka rinci dan cara pengukurannya ada di section 171.

Target per event, payload ditambah frame header, sebelum TLS:

Text message E2E sampai 120 karakter
Maksimum 96 byte overhead ditambah ciphertext

Message receipt delivered atau read
Maksimum 24 byte, memakai watermark kumulatif dan bukan satu receipt per pesan

Typing start atau stop
Maksimum 12 byte, dengan debounce dan coalescing, tidak pernah per ketikan

Presence change
Maksimum 16 byte, hanya saat berubah, diagregasi per room

Room member count update
Maksimum 10 byte, coalesced, jarak minimum 5 detik, hanya delta

PING dan Pong
Maksimum 6 byte

ACK
Maksimum 10 byte

Sync response header
Maksimum 32 byte, lalu hanya range yang hilang

Target per session:

HELLO ditambah Welcome ditambah AUTHENTICATE
Maksimum 512 byte total

Cold start, yaitu auth, profile, 20 chat preview, dan unread count
Maksimum 24 KB

Idle session satu jam tanpa aktivitas
Maksimum 8 KB, hanya heartbeat

Reconnect dengan resume dan tidak ada yang terlewat
Maksimum 400 byte

Aturan yang menghasilkan angka di atas:

Room event menggunakan delta
Media menggunakan object storage dan CDN, tidak melalui chat path
History menggunakan pagination dengan cursor
Images menggunakan thumbnail lebih dahulu
Video menggunakan adaptive bitrate
Voice note menggunakan codec speech dengan bitrate rendah

Transport yang dipakai:

WebSocket dengan binary frame sebagai transport realtime utama
QUIC bila dinegosiasikan
HTTP/2 atau HTTP/3 untuk REST dan media

Yang TIDAK BOLEH dipakai:

Polling setiap beberapa detik untuk data realtime
Server-Sent Events sebagai pengganti WebSocket untuk chat. SSE bersifat satu arah dan berbasis teks, sehingga membatalkan seluruh keuntungan binary protocol. SSE hanya OPSIONAL untuk feed satu arah non-chat seperti admin dashboard
Long polling


57. CLIENT ARCHITECTURE

Next.js:

App Router
TypeScript
Responsive layout
PWA support
Binary WebSocket client, MWP/1
Web Crypto untuk E2E, dengan CryptoKey non-extractable
IndexedDB untuk key material dan cache
Offline outbox yang durable
Service Worker, tanpa cache plaintext pesan
WebRTC untuk voice dan video call
MediaRecorder dan Web Audio untuk voice note
Virtualized message lists
Lazy loading
Code splitting
Strict CSP tanpa inline script dan tanpa eval

Android:

Kotlin
Jetpack Compose
Coroutines
Room database
WorkManager untuk retry upload dan outbox
Foreground service hanya bila benar-benar diperlukan
Android Keystore untuk key non-exportable
Binary WebSocket client, MWP/1, dengan QUIC bila feature bit QUIC dinegosiasikan
WebRTC untuk voice dan video call
Audio recorder untuk voice note

Kedua client memakai protocol yang sama, struct yang sama, opcode yang sama, dan error code yang sama, karena keduanya digenerate dari IDL yang sama di shared/protocol/schema. Tidak ada permukaan protocol khusus per platform. Lihat section 144 dan section 164.


58. UI/UX

Migo harus terasa modern tetapi ringan.

Navigation:

Chats
Rooms
Discover
Games
Notifications
Profile

Mobile:

Bottom navigation

Desktop:

Sidebar
Conversation panel
Details panel


59. CHAT UI

Chat list:

Avatar
Name
Last message
Timestamp
Unread badge
Mute indicator
Pinned indicator

Chat screen:

Messages
Typing
Reply
Attachment
Emoji
Sticker
Gift
Game
Voice note dengan waveform, durasi, dan kontrol kecepatan
Tombol voice call
Tombol video call

Status encryption ditampilkan apa adanya. Private chat dan group chat memakai teks "End-to-end encrypted". Public Room dan Managed Room memakai teks "Encrypted transport, dapat dibaca server untuk moderation". Lihat section 8.

Optimalkan virtualized list untuk ribuan message.


60. ROOM UI

Room header:

Avatar
Room name
Online count
Topic
Search
Room info

Room:

Message list
Member list
Pinned messages
Room controls
Game bot
Gift
Emoji
Reaction


61. MANAGED ROOM DASHBOARD

Owner/Manager dapat membuka:

Overview
Members
Moderators
Permissions
Rules
Reports
Bans
Warnings
Messages
Bots
Games
Announcements
Analytics
Audit logs
Room customization


62. ADMIN PANEL

Global admin panel:

Users
Servers
Regions
Rooms
Managed Rooms
Bots
Reports
Moderation
Economy
Events
Games
System health
Logs
Metrics

Admin tidak boleh dapat membaca E2E private message plaintext.


63. OBSERVABILITY

Setiap server memiliki:

CPU
Memory
Network
Connections
WebSocket count
Message rate
Room count
Online users
Latency
Error rate
Database latency
Replication status

Metrics:

Prometheus-compatible
OpenTelemetry
Structured logging

Metric wajib untuk protocol, aturan sampling tracing, dan daftar hal yang TIDAK BOLEH masuk log ada di section 174. Ringkasnya, plaintext pesan, sealed envelope, key material, raw token, signed URL, IP penuh, SDP, dan ICE candidate tidak pernah ditulis ke log dalam bentuk apa pun.


64. SERVER HEALTH

Node status:

Healthy
Warning
Degraded
Offline
Maintenance

Gateway harus otomatis mengeluarkan node yang unhealthy dari routing.


65. FAILOVER

Jika Asia Gateway gagal:

Client
-> reconnect
-> alternate Asia Gateway

Jika region gagal:

Asia
-> Singapore
-> Japan
-> Hong Kong
-> nearest healthy region

Session harus dapat dipulihkan tanpa kehilangan queued messages.

Session dipulihkan lewat resume pada HELLO. Frame yang belum di-ACK diputar ulang dari buffer sebesar RESUME_BUFFER_FRAMES selama masih dalam RESUME_WINDOW_MS. Bila buffer sudah terlewati, client menerima RESUME_REQUIRED lalu melakukan incremental sync, bukan full resync.

Pesan private tetap dapat dikirim saat partisi karena disimpan di region pengirim lalu diteruskan ketika mesh kembali. Room adalah kasus berbeda: bila region home room tidak terjangkau, room menjadi read-only. Lihat section 150 dan section 170.


66. DATA CONSISTENCY

Tidak semua data membutuhkan strong consistency.

Strong consistency:

Currency
Transactions
Permissions
Account security

Eventual consistency:

Presence
Online count
Room popularity
Leaderboard cache
Some social metrics

Gunakan model consistency sesuai kebutuhan.


67. MESSAGE ORDERING

Ada tiga ruang sequence number yang berbeda di Migo dan ketiganya TIDAK BOLEH dicampur. Rincinya ada di section 152.

Pertama, conversation seq. Diberikan server, monotonic dan tanpa lubang di dalam satu conversation. Ini yang menentukan urutan pesan.

Kedua, frame_seq per session. Dipakai untuk ACK dan resume pada satu koneksi. Tidak ada hubungannya dengan urutan pesan.

Ketiga, federation link sequence. Dipakai untuk replay protection antar node.

Yang dibahas di section ini adalah conversation seq.

Setiap conversation menggunakan sequence number.

Contoh:

1001
1002
1003
1004

Client dapat mendeteksi missing message.

Jika menerima:

1001
1002
1004

Client meminta range yang hilang saja dengan SYNC berisi have_seq 1002 dan to_seq 1003:

1003

Bukan melakukan full resync.

Aturan wajib:

seq diberikan server pada saat pesan diterima, bukan oleh client, karena client tidak dapat dipercaya untuk urutan bersama
seq tidak pernah berubah dan tidak pernah dipakai ulang, termasuk setelah pesan dihapus. Pesan yang dihapus menjadi tombstone dengan seq yang tetap, supaya lubang seq tidak pernah muncul dan client tidak resync tanpa alasan
Urutan tampilan memakai seq, bukan created_at. Clock device tidak dapat dipercaya
created_at hanya untuk tampilan waktu, dan client SEBAIKNYA menampilkannya setelah dikoreksi dengan server_time dari Welcome


68. MESSAGE DEDUPLICATION

Setiap message memiliki unique message ID berukuran 16 byte, dibuat oleh client sebelum pengiriman. UUIDv7 atau ULID, sehingga id juga terurut menurut waktu.

message_id adalah idempotency key. Jika message terkirim ulang akibat reconnect, server mendeteksi duplicate dan membalas MessageAccepted dengan duplicate bernilai true beserta seq dari pengiriman pertama, tanpa membuat pesan baru dan tanpa membuat lubang pada seq.

Ini penting untuk mobile networks yang tidak stabil, karena pada jaringan seluler kegagalan paling umum adalah request yang sebenarnya berhasil tetapi jawabannya tidak sampai.

Dedup pada setiap layer:

Client
Menyimpan message_id yang sudah dirender, sehingga replay setelah resume tidak menghasilkan bubble ganda

Server
Unique constraint pada kombinasi conversation_id dan message_id di database, sehingga dedup tetap benar walaupun dua gateway menerima retry secara bersamaan

Media
Dedup berdasarkan content hash dari byte terenkripsi, sehingga forward voice note yang sama tidak menyalin object

Federation
Dedup berdasarkan kombinasi origin node id dan packet sequence

Push notification
Dedup berdasarkan message_id, supaya satu pesan tidak berbunyi dua kali di device yang sama


69. MEDIA SECURITY

Attachment:

Encrypted transport
Signed upload URL dengan masa berlaku pendek
Signed download URL dengan masa berlaku pendek
File size limits
MIME validation berdasarkan magic byte, bukan hanya berdasarkan header yang dikirim client
Extension validation
Virus dan malware scanning untuk media yang dapat dibaca server
Access control per object, diperiksa sebelum signed URL diterbitkan
Expiration
Private attachment token yang terikat pada account dan device

Aturan wajib:

Media pada private chat dan group chat WAJIB dienkripsi client-side sebelum upload. Yang tersimpan di object storage adalah ciphertext, dan key ada di dalam cryptographic envelope pesan
Voice note pada private chat WAJIB E2E, tanpa pengecualian. Lihat section 167
Bucket TIDAK BOLEH public dan TIDAK BOLEH memiliki URL permanen yang dapat diakses tanpa tanda tangan
Signed URL TIDAK BOLEH ditulis ke log, ke analytics, atau ke crash report, karena URL itu sendiri adalah kredensial
Scanning tidak dapat dilakukan pada media E2E, karena server tidak memiliki plaintext. Untuk media E2E, perlindungan berada di sisi client, yaitu batas ukuran, validasi tipe setelah dekripsi, dan pelaporan oleh user
Media Public Room dan Managed Room dapat dibaca server dan WAJIB melewati scanning


70. SERVER RESOURCE LIMIT

Setiap user memiliki:

Connection limit
Message rate limit
Upload limit
Room join limit
Friend request limit
Bot command limit

Setiap room memiliki:

Member limit
Message rate
Media limit
Bot limit


71. PROTOCOL VERSIONING

STATUS: BUILT untuk aturan versi frame dan decoder. Spesifikasi lengkap ada di section 147.

Protocol memiliki version. Versi saat ini adalah MWP/1, ditandai oleh byte pertama setiap frame bernilai 1.

Perubahan yang diperbolehkan di dalam v1, sehingga tidak memerlukan versi baru:

Menambah opcode baru
Menambah optional field baru pada struct yang sudah ada
Menambah enum variant baru
Menambah feature bit baru
Menambah error code baru

Perubahan yang WAJIB menjadi MWP/2:

Mengubah, mengurutkan ulang, atau menghapus required field
Mengubah arti opcode yang sudah dipakai
Mengubah encoding primitive
Mengubah arti flag bit

Mekanisme backward compatibility:

Optional field yang tidak dikenal dilewati berdasarkan panjangnya. Ini alasan setiap optional entry membawa byte_len, dan ini membuat forward compatibility bekerja tanpa negosiasi apa pun
Enum value yang tidak dikenal didecode menjadi variant Unknown, tidak menjadi error
Opcode yang tidak dikenal dijawab dengan ERROR berisi UNKNOWN_OPCODE, lalu sesi dilanjutkan
Flag bit yang tidak dikenal ditolak, dan ini disengaja. Mengabaikan flag tak dikenal berarti protocol tidak dapat lagi diperluas dengan aman, karena pengirim tidak dapat membedakan penerima yang mengerti dari penerima yang berpura-pura mengerti
Version byte yang tidak didukung dijawab dengan ERROR berisi PROTOCOL_VERSION_UNSUPPORTED lalu koneksi ditutup

Server TIDAK BOLEH crash, hang, panic, atau kehabisan memori karena client lama, client baru, atau client jahat. Setiap batas diperiksa sebelum alokasi.

Saat MWP/2 tiba, server WAJIB melayani v1 dan v2 secara bersamaan selama minimal satu siklus deprecation client penuh. Rencana migrasinya ada di section 176.

Sumber kebenaran protocol adalah IDL di shared/protocol/schema. Kode Rust dan TypeScript dihasilkan dari IDL dan hasil generate ikut di-commit sesuai ADR-0010. Hasil generate yang tidak sinkron dengan IDL adalah kegagalan build melalui make protocol-check, bukan peringatan.


72. FEATURE NEGOTIATION

STATUS: SCHEMA. Bit assignment sudah ada di shared/protocol/schema/meta.json.

Client dan server bertukar bitmask u64 pada HELLO dan Welcome. Session memakai irisan dari keduanya. Server TIDAK BOLEH mengirim frame untuk fitur yang tidak diiklankan client.

Bit yang sudah ditetapkan:

0 COMPRESSION
1 BATCHING
2 E2E_V1
3 GROUP_E2E_V1
4 PRESENCE
5 TYPING
6 ROOMS
7 MEDIA_UPLOAD
8 GAMES
9 BOTS
10 TRANSLATION
11 VOICE_MESSAGE
12 QUIC
13 TRACING
14 RESUME
15 ECONOMY

Bit yang direncanakan. STATUS: SPEC:

16 CALL_V1
17 GROUP_CALL_SFU_V1
18 REACTIONS
19 DELTA_ROOM_STATE
20 MEDIA_RESUMABLE_UPLOAD

Aturan:

Bit yang tidak dikenal diabaikan. Ini satu-satunya tempat Migo mengabaikan hal yang tidak dikenal, dan alasannya berbeda dari flag frame: bitmask fitur bersifat aditif dan tidak mengubah cara frame dibaca, sedangkan flag frame mengubah cara byte diinterpretasikan
Fitur yang tidak dinegosiasikan berarti frame terkait tidak dikirim sama sekali, bukan dikirim lalu diabaikan penerima
Client TIDAK BOLEH menebak dukungan fitur dari versi build server, dari nama node, atau dari percobaan mengirim frame lalu melihat error. Hanya bitmask yang menentukan
Kill switch server dapat mematikan fitur secara global. Ketika fitur dimatikan, server tidak mengiklankan bit tersebut, dan permintaan terkait dijawab FEATURE_DISABLED


73. LOCAL CACHE

Client cache:

Recent conversations
Recent rooms
Profile
Avatar
Room metadata
Recent messages
Stickers
Game assets

Cache memiliki size limit.

User dapat clear cache.


74. STARTUP OPTIMIZATION

Jangan download semuanya ketika aplikasi dibuka.

Startup hanya:

Authenticate
Load profile
Load chat list
Load unread counts
Open realtime connection

Media dan history di-load kemudian.


75. BANDWIDTH MODE

Bandwidth mode dikirim client pada HELLO memakai enum BandwidthMode, sehingga server ikut menyesuaikan laju event yang dihasilkannya:

Auto
Normal
Low Data
Ultra Low Data

Low Data:

No autoplay video
Smaller images
No animated preview
Reduced avatar resolution
Reduced presence updates
Delayed media download
Voice note tidak diunduh otomatis
Call mengutamakan audio, video pada resolusi dan frame rate lebih rendah

Ultra Low Data:

Typing indicator dimatikan
Presence hanya diperbarui pada perubahan penting
Media hanya diunduh atas permintaan eksplisit
Video pada call dimatikan bila jaringan tidak memadai, audio tetap jalan

Mode ini bukan sekadar setting UI. Multiplier interval per mode dan perilaku server ada di section 159, perilaku media call ada di section 166.


76. MOBILE NETWORK SUPPORT

Harus bekerja baik pada:

Wi-Fi
4G
5G
Slow 4G
Unstable network
High latency
Packet loss
Temporary offline

Jangan mengasumsikan koneksi selalu stabil.


77. PUSH NOTIFICATION

Android:

Firebase Cloud Messaging atau provider yang sesuai.

Push payload harus minimum.

Untuk E2E message:

Push tidak berisi plaintext message.

Gunakan generic notification:

"New message"

Client kemudian mengambil encrypted payload melalui koneksi realtime dan melakukan decryption locally.

Untuk voice note, push hanya menyebut "New voice message". Audio TIDAK BOLEH dikirim di dalam push.

Untuk incoming call, push memuat call_id dan penanda call saja, cukup untuk membangunkan aplikasi dan menampilkan UI incoming call. Tidak ada SDP, ICE candidate, atau isi signaling di dalam push.

Push token disimpan dalam bentuk hash dan TIDAK BOLEH ditulis ke log.


78. PRIVACY

Jangan mengumpulkan data yang tidak diperlukan.

Minimalkan:

IP retention
Location
Device fingerprint
Message metadata

Sediakan privacy controls.


79. USER IDENTITY

Gunakan unique immutable User ID.

Username dapat berubah jika sistem mengizinkan.

Contoh:

User ID:
MGO-7F82A91C

Username:
@satoshi


80. USERNAME SYSTEM

Username:

Unique
Case-insensitive
Reserved names
Rate limited changes
Anti-impersonation

Verified accounts dapat memiliki verification badge.


81. ROOM ID

Room memiliki immutable ID.

Contoh:

MGO-ROOM-82F91A

Nama room dapat berubah tanpa merusak links.


82. DEEP LINK

Support:

migo://user/username
migo://room/roomid
migo://chat/chatid
migo://game/gameid

Web fallback:

https://migo.example/user/username
https://migo.example/room/roomid


83. PUBLIC ROOM DISCOVERY

Ranking jangan hanya berdasarkan member count.

Gunakan:

Active users
Message quality
Retention
Reports
Spam rate
Engagement
Room health
Moderation quality

Room dengan banyak bot spam tidak boleh otomatis menjadi trending.


84. MANAGED ROOM VERIFICATION

Room dapat memiliki:

Verified
Official
Community
Creator

Verified room mendapatkan badge.


85. ROOM OWNERSHIP

Owner dapat:

Transfer ownership
Add managers
Remove managers
Configure permissions
Delete room
Archive room

Transfer ownership harus membutuhkan re-authentication.


86. BOT MARKETPLACE

Migo dapat memiliki Bot Directory:

Games
Utility
Moderation
Translation
Entertainment
Community

User dapat menambahkan bot ke room berdasarkan permission.

Bot harus melewati permission review.


87. GAME ECONOMY

Game dapat menghasilkan:

XP
Points
Non-monetary rewards
Cosmetics

Jika memakai Coins/Gems, gunakan strict anti-abuse rules.

Jangan membuat sistem yang menyerupai gambling atau memungkinkan cash-out tanpa regulatory review.


88. GAME BOT COMMAND EXAMPLE

Room:

/games

Bot:

Available Games:
1. Trivia
2. RPS
3. 2048
4. Guess
5. Chess

User:

/rps rock

Bot:

You chose Rock.
Bot chose Scissors.
You win.


89. GAME STATE

Game state server-authoritative.

Client hanya mengirim action.

Server memvalidasi:

Player
Turn
Action
Cooldown
Game state

Client tidak boleh menentukan hasil game sendiri.


90. ANTI-CHEAT

Untuk game:

Server authoritative
Randomness server-side
Signed game events
Rate limits
Action validation
Replay detection

Jangan percaya score dari client.


91. SOURCE CODE STRUCTURE

Struktur berikut adalah struktur nyata di repository, bukan rencana.

migo/
  migo.md
  README.md
  CONTRIBUTING.md
  SECURITY.md
  LICENSE
  Makefile
  .env.example
  server/
    Cargo.toml
    migrations/
    crates/
      migo-core/
      migo-wire/
      migo-protocol/
      migo-crypto/
      migo-store/
      migo-cache/
      migo-ratelimit/
      migo-auth/
      migo-messaging/
      migo-presence/
      migo-rooms/
      migo-social/
      migo-media/
      migo-moderation/
      migo-notify/
      migo-economy/
      migo-games/
      migo-bots/
      migo-federation/
      migo-gateway/
      migo-api/
      migod/
    tests/
  shared/
    protocol/
      schema/
      vectors/
  packages/
    wire/
    protocol/
    crypto/
    sdk/
  clients/
    web/
    android/
  infra/
    compose/
    docker/
    kubernetes/
    terraform/
  tools/
    protocol-codegen/
    scripts/
    loadgen/
  docs/
    adr/
    runbooks/
  tests/
    e2e/

Aturan struktur:

Server adalah modular monolith dengan role composition. Satu binary bernama migod, dan role diaktifkan lewat konfigurasi MIGO_NODE__ROLES, misalnya api, gateway, room, game, dan federation. Lihat ADR-0001.
Dependency antar crate hanya boleh mengarah ke bawah, dari layer aplikasi ke layer dasar. Tidak ada dependency melingkar.
shared/protocol/schema adalah satu-satunya sumber kebenaran wire protocol. Kode Rust di server/crates/migo-protocol/src/generated.rs dan kode TypeScript di packages/protocol dihasilkan dari schema tersebut dan ikut di-commit.
shared/protocol/vectors berisi test vector biner yang WAJIB menghasilkan byte identik pada Rust dan TypeScript.
clients/web adalah Next.js. clients/android adalah Kotlin. Keduanya memakai protocol yang sama melalui packages/sdk dan implementasi Kotlin yang setara.
Tidak ada secret di dalam repository. .env.example di-commit, .env tidak.


92. RUST BACKEND SERVICES

Gunakan Rust untuk service yang membutuhkan:

High concurrency
Low latency
Low memory usage
Network processing
Realtime messaging
Protocol handling
Call signaling
Server federation
Game servers

Komponen media yang berdiri di luar proses utama:

STUN, untuk NAT traversal
TURN, sebagai relay terenkripsi ketika P2P gagal
SFU, untuk group call, meneruskan media tanpa akses plaintext

TURN dan SFU tidak menyentuh plaintext media dan karena itu tidak perlu berada di dalam migod. Keduanya diskalakan terpisah karena profil bebannya adalah bandwidth, bukan logika aplikasi.

Pisahkan service secara modular.

Jangan langsung membuat puluhan microservices jika belum diperlukan.

Mulai dari modular monolith yang dapat dipecah menjadi service ketika workload meningkat. Peran diaktifkan lewat komposisi role, bukan lewat build terpisah. Lihat ADR-0001 dan section 91.


93. RUST TECHNOLOGY DIRECTION

Gunakan ecosystem Rust yang mature untuk:

Async runtime
HTTP
WebSocket
QUIC
Serialization
Database
Cryptography
Observability

Hindari dependency yang tidak terawat.

Audit dependency menggunakan cargo tooling.


94. TESTING

Wajib ada:

Unit tests
Integration tests
Protocol tests
Conformance vector tests lintas bahasa
Codec fuzz tests
Byte size tests terhadap budget
Backward compatibility tests
Crypto tests
E2E tests
Load tests
Stress tests
Property tests
Mobile network tests
Reconnect tests
Offline tests
Multi-region tests
Failover tests
Security tests

Detail untuk protocol ada di section 172, untuk kegagalan multi-region ada di section 173. Test yang menyentuh waktu, keacakan, dan jaringan WAJIB memakai Clock, Random, dan Transport yang diinjeksi, sesuai ADR-0009, agar kegagalan dapat direproduksi.


95. SECURITY TESTING

Test:

Authentication bypass
Authorization bypass
Room permission bypass
IDOR
Replay attack
Message duplication
Rate limit bypass
Spam
Flood
Malformed packets
Invalid protocol
Oversized payload
Memory exhaustion
WebSocket abuse
Bot abuse
File upload abuse
Session hijacking
Token theft
Key handling

Test keamanan khusus protocol, termasuk penolakan frame Server-auth dari socket client, mesh handshake tanpa signature valid, replay paket federation, deteksi refresh token reuse, dan penolakan start production dengan secret default, ada di section 172.


96. MULTI-REGION TESTING

Simulasikan:

Asia down
Europe down
US down
Network partition
Packet loss
High latency
Server overload
Database failure
Redis failure
Object storage failure

Migo harus tetap berfungsi atau degrade gracefully.

Sepuluh skenario kegagalan multi-region beserta ekspektasi masing-masing ada di section 173. Semuanya WAJIB dapat dijalankan sebagai simulasi deterministik, karena kegagalan yang tidak dapat direproduksi tidak dapat diperbaiki dengan percaya diri.


97. LOAD TESTING

Test scenario:

100 users
1,000 users
10,000 users
100,000 users
1,000,000 concurrent users

Test:

Private chat
Public room
Managed room
Presence
Game bots
Media upload
Global room

Setiap skenario load juga diukur byte-nya, bukan hanya latensi dan throughput. Skenario yang melewati budget di section 56 dan section 171 lebih dari 10 persen menggagalkan CI. Regression bandwidth yang tidak terdeteksi akan muncul sebagai keluhan kuota pengguna, bukan sebagai alert.


98. LARGE ROOM TEST

Simulasikan:

10,000 users
50,000 users
100,000 users

Satu room tidak boleh menyebabkan seluruh cluster mati.

Gunakan room sharding dan regional fanout.


99. DEPLOYMENT

Setiap region dapat memiliki:

Gateway
Realtime servers
Room servers
Game servers
Cache
Database replicas
Object storage integration

Global control plane mengelola:

Node discovery
Configuration
Health
Routing
Deployment

Urutan rollout, aturan bahwa server selalu lebih dulu daripada client, kill switch per feature, dan prosedur drain ada di section 175. Strategi migrasi protocol ada di section 176.


100. SERVER JOIN PROCESS

Node baru:

Generate identity
Register
Authenticate
Receive configuration
Perform health check
Join federation
Sync routing metadata
Become available

Tidak boleh langsung menerima production traffic sebelum health check.


101. SERVER LEAVE PROCESS

Graceful shutdown:

Stop new connections
Finish active operations
Flush queues
Transfer room ownership if needed
Notify federation
Close connections
Shutdown


102. CONFIGURATION

Gunakan environment variable dan config file.

Presedensi, dari terendah ke tertinggi:

Built-in default
config/*.toml, dipilih lewat MIGO_CONFIG
Environment variable
CLI flag

Semua key memakai prefix MIGO_ dan nested key memakai double underscore, sehingga MIGO_STORE__URL memetakan ke store.url.

Contoh:

MIGO_NODE__ID
MIGO_NODE__REGION
MIGO_NODE__COUNTRY
MIGO_NODE__ROLES
MIGO_NODE__ENVIRONMENT
MIGO_NODE__SIGNING_KEY
MIGO_HTTP__BIND
MIGO_HTTP__PUBLIC_URL
MIGO_QUIC__BIND
MIGO_STORE__BACKEND
MIGO_STORE__URL
MIGO_CACHE__BACKEND
MIGO_CACHE__URL
MIGO_MEDIA__BACKEND
MIGO_MEDIA__BUCKET
MIGO_AUTH__TOKEN_KEY
MIGO_TELEMETRY__LOG_LEVEL
MIGO_TELEMETRY__METRICS_BIND

Key yang tidak dikenal adalah error, bukan diabaikan. MIGO_STOR__URL adalah salah tulis, dan salah tulis yang diabaikan berarti server berjalan dengan nilai default sementara operator yakin sudah mengubahnya.

Peran node ditentukan oleh MIGO_NODE__ROLES, bukan oleh build terpisah. Satu binary migod menjalankan komposisi role yang berbeda per deployment. Lihat ADR-0001.

Secret jangan masuk Git. File .env di-ignore, dan .env.example yang di-commit hanya memuat placeholder tanpa nilai nyata. Lihat section 103.


103. SECRET MANAGEMENT

Gunakan:

Environment variable
Secret manager
Key rotation

Jangan menyimpan:

Database password
Token signing secret
Node private key
Push provider credential
TURN static secret
Object storage credential

di source code.

Migo memakai opaque token berbasis HMAC-SHA256, bukan JWT, sehingga yang dilindungi adalah satu signing secret tanpa permukaan pemilihan algoritma. Lihat section 162.

migod WAJIB menolak start pada environment production bila secret kosong atau masih bernilai default development. Gagal saat start jauh lebih baik daripada berjalan dengan secret yang dapat diduga.


104. BACKUP

Backup:

User metadata
Room metadata
Configuration
Economy ledger
Moderation data

E2E private message plaintext tidak tersedia untuk backup server karena encryption dilakukan client-side.


105. DISASTER RECOVERY

Tetapkan:

RPO
RTO
Backup frequency
Cross-region backup
Restore testing

Backup harus diuji restore secara berkala.


106. ACCOUNT RECOVERY

E2E encryption harus memperhatikan recovery, dan recovery TIDAK BOLEH dicapai dengan memberi server kemampuan membaca pesan.

Pilihan yang diperbolehkan:

Recovery material yang dipegang user, misalnya recovery code yang hanya ada di tangan user
Multi-device key synchronization antar device milik user sendiri, dienkripsi dari device ke device
Passkey-assisted recovery, di mana kunci pembuka tetap berada pada authenticator user
Encrypted key backup yang dienkripsi di client dengan kunci turunan dari material milik user

Yang tidak diperbolehkan:

Recovery yang dapat dijalankan server sendiri tanpa material dari user
Key escrow, master key, atau recovery key milik server
Menurunkan kunci backup dari data yang sudah dimiliki server, misalnya nomor telepon atau alamat email saja

User harus diberi peringatan bahwa kehilangan recovery material dapat menyebabkan kehilangan akses ke encrypted history. Peringatan ini WAJIB muncul sebelum user bergantung pada history, bukan setelah kehilangan terjadi.


107. CLIENT KEY BACKUP

Backup key bersifat opt-in dan sepenuhnya dienkripsi di client.

Jika pengguna mengaktifkan backup:

Private encryption key dienkripsi client-side terlebih dahulu, memakai kunci yang diturunkan dari recovery material milik user dengan Argon2id.

Server menyimpan:

Encrypted key backup

Server tidak mengetahui plaintext key dan tidak memiliki jalur apa pun untuk membukanya.

Aturan tambahan:

Percobaan membuka backup dibatasi laju di server, karena passphrase manusia lebih lemah daripada kunci acak
Server TIDAK BOLEH menyimpan salinan recovery material atau hash yang cukup untuk melakukan brute force offline
Jika backup dimatikan, objek backup dihapus, bukan hanya disembunyikan
Backup yang tidak dapat dibuka tidak dapat diselamatkan server. Ini konsekuensi langsung dari tidak adanya key escrow, dan WAJIB dijelaskan ke user


108. WEB E2E SECURITY

Web client menggunakan:

Web Crypto API
Secure context HTTPS
IndexedDB
Strict CSP tanpa inline script dan tanpa eval
Trusted Types bila memungkinkan
Strict dependency policy

Private key disimpan sebagai CryptoKey non-extractable di IndexedDB.

Private key TIDAK BOLEH disimpan di localStorage, sessionStorage, cookie, URL, atau di dalam variabel global yang dapat diakses script lain, baik plaintext maupun hasil encoding.

Operasi kriptografi dilakukan lewat Web Crypto. Primitive kriptografi yang ditulis tangan dalam JavaScript TIDAK BOLEH dipakai di production.

Service Worker TIDAK BOLEH menyimpan cache plaintext pesan.

Satu celah XSS cukup untuk menyalahgunakan kunci non-extractable meski kunci itu tidak dapat diekspor, karena penyerang dapat memakainya di halaman yang sama. Karena itu CSP bukan pelengkap, tetapi bagian dari model keamanan E2E di web. Lihat section 164.


109. ANDROID E2E SECURITY

Android:

Android Keystore dengan key yang tidak dapat diekspor
Encrypted local database
Secure storage
Screenshot policy pada layar safety number dan verifikasi key
Biometric unlock
App lock

Private key dibuat di dalam Keystore dan TIDAK BOLEH disimpan di SharedPreferences, file biasa, log, atau backup OS, baik plaintext maupun hasil encoding.

Key material WAJIB dikecualikan dari backup dan transfer perangkat otomatis. Backup key hanya boleh terjadi lewat jalur yang dienkripsi di client sesuai section 107.

Kehilangan perangkat berarti kehilangan kunci. Tidak ada jalan pintas di server, karena server tidak memegang kunci apa pun. Lihat section 164.


110. ANDROID BACKGROUND

Android harus mampu:

Receive push
Reconnect efficiently
Sync messages
Upload queued media
Process notifications

Jangan menggunakan permanent background service hanya untuk menjaga WebSocket hidup karena akan boros baterai.

Gunakan push + reconnect saat aplikasi aktif.


111. WEB PERFORMANCE

Next.js harus:

Code split
Lazy load
Virtualize long lists
Optimize images
Cache static assets
Use CDN
Avoid unnecessary hydration
Use server rendering jika cocok
Minimize JavaScript bundle

Chat interface harus tetap smooth pada device low-end.


112. ANDROID PERFORMANCE

Target:

Low memory usage
Low battery usage
Low network usage
Fast startup
Efficient Recycler/Compose list
Efficient image cache
Background work minimized


113. ACCESSIBILITY

Support:

Screen reader
Large text
High contrast
Keyboard navigation
Reduced motion
Accessible buttons
VoiceOver
TalkBack


114. INTERNATIONALIZATION

Migo harus siap untuk:

English
Indonesian
Japanese
Korean
Chinese
Spanish
Portuguese
French
German
Arabic
Hindi

Semua UI string harus menggunakan localization system.

Jangan hardcode text UI.


115. TIMEZONE

Server menyimpan timestamp dalam UTC.

Client menampilkan local timezone.

Room events mengikuti timezone user atau room.


116. ANALYTICS

Collect minimal privacy-conscious analytics:

Crash
Latency
Feature usage
Performance
Retention

Jangan mengumpulkan private E2E message content.

Yang juga TIDAK BOLEH masuk analytics:

Plaintext pesan, plaintext audio voice note, dan transcript
Sealed envelope atau ciphertext
Key material dalam bentuk apa pun
Signed URL, karena URL itu sendiri adalah kredensial
Raw token, password, dan IP address penuh
SDP dan ICE candidate

Metrik call dan quality score dikumpulkan dalam bentuk agregat, tanpa isi call. Lihat section 174.


117. CRASH REPORTING

Web:

Error monitoring

Android:

Crash reporting

Error log harus:

Redact secrets
Redact tokens
Redact key material
Redact private message content
Redact signed URL
Redact SDP dan ICE candidate
Truncate IP address ke kelas jaringan

Crash report TIDAK BOLEH memuat dump memori yang berisi key material atau plaintext pesan. Daftar lengkap yang tidak boleh dicatat ada di section 174.


118. API DESIGN

Migo memiliki dua permukaan yang terpisah dan sengaja berbeda.

Pertama, realtime surface. Binary MWP/1 di atas WebSocket atau QUIC. Semua chat, room, presence, typing, reaction, friend event, notification, voice note signaling, call signaling, game event, dan bot command berjalan di sini. JSON TIDAK BOLEH dipakai di sini.

Kedua, REST surface berbasis JSON. Hanya untuk hal yang bukan realtime.

REST JSON diperbolehkan untuk:

Authentication bootstrap, yaitu register, login, refresh, logout
Media upload dan media authorization, karena upload byte besar memang milik HTTP
Public API untuk integrasi pihak ketiga
Bot API untuk bot yang tidak memakai socket
Admin dan moderation panel
Configuration dan feature flag administration
Health, readiness, dan metrics endpoint
Development dan debugging tooling

REST JSON TIDAK BOLEH dipakai untuk:

Mengirim atau menerima chat message
Mengambil pesan baru dengan polling
Presence, typing, reaction, dan seluruh event realtime
Call signaling
Federation

Aturan bersama kedua permukaan:

Payload struct yang sama dipakai di kedua permukaan. REST hanya merepresentasikan struct yang sama dalam JSON, sehingga tidak ada dua model data yang harus disinkronkan manual
Error code dan symbol sama di kedua permukaan. Pemetaan ke HTTP status berasal dari tabel yang digenerate dari schema/errors.json, bukan dari keputusan per handler
Semua listing memiliki pagination dengan batas maksimum server-side
Semua operasi yang mengubah state menerima idempotency key
Versi REST API ada di path, misalnya /v1

Public API:

Authentication
Users
Profiles
Rooms
Managed Rooms
Friends
Messages
Media
Gifts
Games
Bots
Events

Internal federation API berbeda dari public API dan memakai binary protocol, bukan REST. Jangan expose internal server endpoint dan federation listener ke public Internet. Keduanya berada di network segment terpisah dengan allow-list.


119. API AUTHORIZATION

Setiap request:

Authenticate
Authorize
Validate input
Rate limit
Execute
Audit sensitive operation

Jangan mengandalkan client-side permission checks.


120. RATE LIMITING ARCHITECTURE

Rate limit berdasarkan:

IP
User ID
Device
Token
Room
Bot
Endpoint

Gunakan distributed rate limit jika diperlukan.


121. DOS PROTECTION

Layer:

CDN
Edge rate limit
Gateway rate limit
Connection limit
Request size limit
Message rate limit
Room rate limit
Bot rate limit

Server harus menolak payload abnormal lebih awal.


122. FILE UPLOAD LIMIT

Tetapkan limit:

Avatar
Image
Video
Audio
Voice note, dengan batas durasi default 5 menit dan dapat dikonfigurasi
Document

Validasi server-side.

Jangan percaya Content-Type dari client. Tipe file diverifikasi dari magic byte, bukan dari header yang dikirim client.

Untuk media E2E, server hanya melihat ciphertext, sehingga validasi isi tidak mungkin dilakukan server. Yang tetap divalidasi server adalah ukuran, kuota, laju, dan otorisasi. Validasi isi berpindah ke client. Konsekuensi ini disebutkan secara terbuka di section 69 dan section 168.


123. LINK SAFETY

URL dalam chat dapat diproses:

Normalization
Domain reputation
Malicious URL detection
Phishing detection

Namun jangan membaca encrypted private chat plaintext di server.

Moderation otomatis dapat diterapkan pada Public/Managed Rooms sesuai privacy policy.


124. PRIVACY MODES

User dapat memilih:

Everyone
Friends
Nobody

Untuk:

Messages
Friend requests
Profile
Online status
Last seen
Gifts
Room invitations


125. ACCOUNT DELETE

Account deletion harus:

Re-authenticate
Confirm
Schedule deletion
Revoke sessions
Delete personal data sesuai policy
Handle room ownership
Handle managed rooms
Handle economy state
Handle moderation records sesuai legal requirements


126. MIGRATION

Ada empat jenis migrasi di Migo dan keempatnya membutuhkan rencana yang berbeda.

Database migration WAJIB:

Versioned
Tested
Reversible where possible
Backup before migration
Forward compatible dengan versi aplikasi sebelumnya, sehingga rollout dan rollback tidak memerlukan downtime
Dijalankan dalam dua langkah untuk perubahan yang merusak, yaitu tambah kolom baru lalu tulis ke keduanya, baru hapus kolom lama pada rilis berikutnya
File migration yang sudah pernah diterapkan bersifat immutable. Perbaikan dilakukan dengan migration baru, bukan dengan mengubah file lama

Jangan melakukan destructive migration tanpa migration plan.

Protocol migration mengikuti section 176. Ringkasnya, perubahan additive terjadi di dalam MWP/1, dan perubahan yang merusak memerlukan MWP/2 dengan masa dual-speak.

Client migration:

Client lama WAJIB tetap berfungsi selama satu siklus deprecation penuh
Versi client minimum yang didukung diumumkan lewat Welcome dan RECONNECT_HINT, bukan dengan memutus koneksi tanpa penjelasan
Local database di client memiliki schema version sendiri dan migrasi lokal yang idempotent, karena user dapat melompati beberapa versi aplikasi

Key material migration:

Perubahan format penyimpanan key di device WAJIB dapat membaca format lama, bukan meminta user membuat identitas baru
Kehilangan key berarti kehilangan riwayat pesan yang terenkripsi. Tidak ada jalan pintas di server, karena memang tidak ada key di server


127. DEVELOPMENT PHASE

Phase 0:

Wire protocol MWP/1
IDL di shared/protocol/schema
Codegen Rust dan TypeScript
Conformance vector lintas bahasa
Codec fuzz


Phase 1:

Authentication
User profile
1-on-1 chat
E2E encryption
Web client
Android client
Basic Rust backend
Basic multi-region gateway


Phase 2:

Friend system
Group chat
Public Room
Room discovery
Presence
Notifications


Phase 3:

Managed Room
Roles
Moderation
Admin panel
Reports
Ban/mute


Phase 4:

Virtual gifts
Coins
XP
Levels
Badges
Achievements


Phase 5:

Game Bot Framework
Mini games
Game leaderboard
Room games


Phase 6:

Social feed
Events
Global translation
Advanced discovery


Phase 7:

Multi-region federation
Server mesh
Automatic failover
Room sharding
Advanced scaling


Phase 8:

Voice note
Encrypted resumable media upload
Waveform dan playback control
Offline voice note queue


Phase 9:

Call signaling
Voice call 1-on-1 dengan P2P dan E2E
Video call 1-on-1
STUN dan TURN regional
Group call dengan SFU


Phase 0 mendahului semua phase lain karena kedua client dan seluruh mesh berbicara dalam protocol yang sama. Mengubah wire format setelah dua client ditulis jauh lebih mahal daripada menyelesaikannya lebih dahulu.


128. MVP FEATURES

MVP wajib memiliki:

Account
Profile
1-on-1 E2E chat
Group chat
Friends
Public Room
Managed Room
Room moderation
Presence
Typing
Notifications
Basic media
Binary protocol MWP/1
Offline-first outbox dan incremental sync
Android
Next.js Web
Rust backend
Multi-region gateway
Server authentication
Automatic reconnect dengan resume


129. POST-MVP

Setelah MVP stabil:

Voice note
Voice call
Video call
Group call dengan SFU
Advanced bots
More games
Virtual economy
Gifts
Social feed
Events
Translation
Advanced avatar
Creator system
Bot marketplace
Advanced analytics

Voice note, voice call, dan video call sudah memiliki spesifikasi protocol lengkap di section 165 sampai section 167 dan sudah memiliki tempat di IDL, tetapi belum diimplementasikan. Status per bagian ada di section 177.


130. DEVELOPMENT PRINCIPLE

Jangan mengejar jumlah fitur sebelum core system stabil.

Urutan prioritas:

Security
Reliability
Messaging
E2E encryption
Bandwidth efficiency
Room system
Moderation
Multi-region
Performance
Economy
Games
Social features


131. CORE DESIGN PRINCIPLE

Migo harus memiliki karakter:

Fast
Lightweight
Low bandwidth
Low battery
Secure
Global
Community-focused
Room-centric
E2E by default untuk private communication
Highly moderated public communities
Extensible bot system
Multi-region
Open protocol architecture


132. FINAL PRODUCT STRUCTURE

Migo
|
+-- 💬 Chats
|   +-- 1-on-1
|   +-- Group
|   +-- Media
|   +-- Voice messages
|   +-- Gifts
|
+-- 🌎 Rooms
|   +-- Public Rooms
|   +-- Managed Rooms
|   +-- Country
|   +-- Language
|   +-- Gaming
|   +-- Technology
|   +-- Music
|   +-- Community
|
+-- 👥 Social
|   +-- Friends
|   +-- Followers
|   +-- Profiles
|   +-- Feed
|
+-- 🎮 Games
|   +-- Game Bots
|   +-- Mini Games
|   +-- Leaderboards
|   +-- Achievements
|
+-- 🎁 Economy
|   +-- Gifts
|   +-- Coins
|   +-- Gems
|   +-- Cosmetics
|
+-- 🏆 Progress
|   +-- XP
|   +-- Levels
|   +-- Badges
|   +-- Reputation
|
+-- 🎉 Events
|
+-- 🔔 Notifications
|
+-- 🛡️ Moderation
|
+-- ⚙️ Settings
|
+-- 🔐 Security
|   +-- E2E
|   +-- Devices
|   +-- Passkeys
|   +-- 2FA
|
+-- 🌐 Global Infrastructure
    +-- Asia
    +-- Europe
    +-- US
    +-- Server Mesh
    +-- Routing
    +-- Failover
    +-- Federation


133. CORE REQUIREMENT

Seluruh implementasi harus production-oriented dan tidak boleh hanya membuat prototype UI.

Setiap fitur harus memiliki:

Frontend
Backend
Protocol, yaitu opcode dan struct di shared/protocol/schema, bukan payload ad hoc
Database model
Authorization
Validation
Error handling dengan error code dari registry, bukan string bebas
Offline handling
Reconnect handling
Rate limiting dengan cost yang dideklarasikan di IDL
Security handling
Logging
Metrics
Testing

Setiap fitur yang menyentuh wire protocol harus memiliki tambahan:

Binary conformance vector di shared/protocol/vectors, dan Rust serta TypeScript WAJIB menghasilkan byte identik
Round-trip test encode lalu decode
Fuzz test decoder untuk opcode dan struct baru
Assertion ukuran byte terhadap budget di section 56 dan section 171
Test kompatibilitas dengan client versi sebelumnya, yaitu optional field baru dilewati dengan benar oleh decoder lama

Setiap fitur yang menyentuh E2E harus memiliki tambahan:

Test yang membuktikan server tidak pernah menerima plaintext
Test bahwa associated data mengikat metadata, sehingga ciphertext tidak dapat dipindah ke conversation lain
Test rotasi key dan test bahwa member yang keluar tidak dapat membaca pesan berikutnya

Setiap UI harus memiliki:

Loading state
Empty state
Error state
Offline state
Permission denied state
Retry state
Success state
Accessibility
Responsive layout
Label encryption yang jujur sesuai section 8


134. AUTO TEST AND AUTO FIX

Setiap modul harus dibuat dengan automated testing.

Workflow:

Implement
Build
Lint
Unit test
Integration test
Protocol test
Security test
Load test
Fix errors
Run tests again
Repeat until stable

Jangan menyatakan fitur selesai hanya karena build berhasil.

Fitur dianggap selesai apabila:

Build berhasil
Test berhasil
Tidak ada known critical bug
Permission benar
Offline behavior benar
Reconnect behavior benar
Error handling benar
Security checks berhasil
Bandwidth behavior sesuai target


135. FINAL OBJECTIVE

Bangun Migo sebagai platform messenger dan community global modern yang mempertahankan konsep terbaik messenger komunitas klasik:

Chat 1-on-1
Group Chat
Voice note
Voice call
Video call
Public Room
Managed Room
Global chat
Friend system
Profile
Avatar
Virtual gifts
Virtual currency
Game bots
Mini games
Community
Events

tetapi menggunakan teknologi modern:

Rust backend
Multi-region server mesh
Secure server-to-server federation dengan binary federation packet
Automatic E2E encryption
Binary-first protocol MWP/1 untuk seluruh realtime communication
P2P-first WebRTC media dengan E2E untuk voice dan video call
SFU dengan E2E untuk group call
TURN encrypted fallback ketika P2P gagal
Next.js responsive web client
Native Android client
Offline-first architecture
Efficient media delivery dengan signed temporary URL
Resumable encrypted media upload
Automatic reconnect dengan exponential backoff dan jitter
Server failover
Room sharding
Strong moderation
Bot sandbox
Game engine
Modern security

Tujuan akhirnya adalah Migo terasa ringan seperti messenger era klasik, tetapi secara arsitektur mampu berkembang menjadi platform global dengan jutaan pengguna, ribuan Public Room, Managed Room berskala besar, game bots, voice note, voice call, video call, dan komunikasi terenkripsi yang aman.

Spesifikasi protocol yang menjadi dasar seluruh poin di atas ada pada bagian 136 sampai 178. Bagian tersebut bersifat normative: jika bagian produk 1 sampai 135 dan bagian protocol 136 sampai 178 berbeda mengenai perilaku protocol, bagian protocol yang berlaku. Aturan lengkapnya ada pada bagian 0.


136. PROTOCOL OVERVIEW: MWP/1

STATUS: BUILT untuk framing dan codec. Kode ada di server/crates/migo-wire dan packages/protocol.

Migo Wire Protocol versi 1, disingkat MWP/1, adalah binary protocol untuk seluruh komunikasi realtime Migo. Satu protocol dipakai oleh web client, Android client, bot, dan federation antar node, sehingga tidak ada dua dialek yang harus dijaga tetap sinkron.

Tiga sifat yang menentukan seluruh desain:

Hemat byte. Header minimum 4 byte. Required field tidak membayar overhead framing sama sekali.
Dapat berkembang tanpa negosiasi. Optional field membawa panjangnya sendiri, sehingga penerima lama melewati field yang tidak dikenal.
Aman terhadap input jahat. Setiap batas diperiksa sebelum alokasi memori.

Lapisan protocol dari bawah ke atas:

Transport, yaitu WebSocket, QUIC, atau TCP, semuanya di atas TLS 1.3
Frame, yaitu MWP/1 envelope pada section 139
Payload, yaitu MSE struct pada section 143
Cryptographic envelope untuk konten private, pada section 11 dan section 163

Lapisan frame dan lapisan cryptographic envelope sengaja dipisah. Server memproses lapisan frame dan tidak dapat memproses lapisan cryptographic envelope. Pemisahan ini yang membuat E2E dan routing dapat berjalan bersama.


137. BINARY-FIRST MANDATE

STATUS: BUILT untuk framing biner di migo-wire. Mandat pada bagian ini bersifat normatif dan mengikat seluruh dokumen, termasuk section 1 sampai 135.

Ini requirement wajib, bukan preferensi.

Yang WAJIB binary:

Chat 1-on-1
Private message
Group message
Public Room message
Managed Room message
Presence
Typing indicator
Reaction
Friend event
Voice note signaling dan metadata
Game event
Bot command
Notification
WebRTC call signaling
ACK, retry, dan synchronization
Server-to-server multi-region federation

Yang boleh JSON:

REST dan public API
Configuration file
Admin dan debugging tooling
Test fixture yang dibaca manusia
Metrics exposition dan log

Yang TIDAK BOLEH:

JSON sebagai wire protocol realtime
Text frame WebSocket
MessagePack dan CBOR pada realtime path, karena keduanya self-describing sehingga nama field terkirim pada setiap pesan
Base64 pada realtime path. Base64 menambah 33 persen dan tidak diperlukan pada transport biner
XML dan GraphQL pada realtime path
Long polling dan polling berkala untuk data realtime

Alasan angkanya. Sebuah pesan chat dengan empat id, satu timestamp, dan tiga enum: dalam JSON dengan id berbentuk string dan timestamp ISO-8601, metadata saja menghabiskan sekitar 300 byte sebelum isi pesan. Dalam MWP/1 metadata yang sama sekitar 80 byte. Pada skala jutaan pesan per menit, selisih itu adalah biaya bandwidth, biaya baterai, dan latensi pada jaringan seluler lambat.


138. TRANSPORT BINDINGS

STATUS: BUILT untuk WebSocket, length-prefixed stream, dan listener QUIC opsional (diaktifkan lewat MIGO_QUIC__BIND; bit QUIC hanya diiklankan saat listener aktif). STATUS: SPEC untuk QUIC datagram dan jalur data QUIC pada client.

WebSocket:

Satu MWP frame per satu binary WebSocket message
Text frame TIDAK BOLEH dipakai. Menerima text frame adalah protocol violation dan koneksi ditutup
WebSocket message boundary yang menyediakan panjang frame, sehingga frame tidak membawa length field sendiri
permessage-deflate WAJIB dimatikan, karena keputusan kompresi diambil per frame oleh MWP pada section 155

QUIC (opsi kedua):

Satu MWP frame per QUIC datagram bila datagram tersedia, dengan batas ukuran sesuai path MTU
Untuk QUIC stream, framing memakai length prefix u32 big-endian diikuti frame
Dinegosiasikan melalui feature bit QUIC. Server mengiklankan bit QUIC hanya bila listener QUIC diaktifkan lewat konfigurasi; TCP/WebSocket tetap menjadi transport default
Keuntungan utamanya adalah tidak ada head-of-line blocking dan reconnect yang lebih cepat saat berpindah jaringan

TCP:

Length prefix u32 big-endian diikuti frame. Dipakai untuk federation dan untuk testing

HTTP:

REST fallback membawa payload struct yang sama dalam bentuk JSON, hanya untuk development, admin, dan bot

Aturan umum:

TLS 1.3 wajib pada semua transport. Tidak ada transport plaintext, termasuk di development
Panjang frame maksimum MAX_FRAME_BYTES yaitu 262144 byte, diperiksa sebelum alokasi buffer


139. PACKET ENVELOPE

STATUS: BUILT untuk header dasar. STATUS: SPEC untuk metadata block pada section 141.

Layout frame:

 0        1        2..       ..        ..                        end
+--------+--------+---------+----------+---------------+---------+
|version | flags  | opcode  | correl.  | flag headers  | payload |
|  u8    |  u8    | varint  |  varint  |  opsional     |  bytes  |
+--------+--------+---------+----------+---------------+---------+

Penjelasan setiap elemen:

version, u8
Bernilai 1 untuk MWP/1. Version yang tidak dikenal dijawab ERROR PROTOCOL_VERSION_UNSUPPORTED lalu koneksi ditutup. Server TIDAK BOLEH panic karena byte ini.

flags, u8
Delapan bit yang menentukan header opsional dan perlakuan payload. Lihat section 140.

opcode, varint
Packet type. Ini adalah field "packet type" yang diminta oleh requirement. Registry lengkap ada di section 145. Opcode yang sering dipakai sengaja diberi nilai di bawah 128 supaya cukup satu byte.

correlation, varint
Ini adalah field "request ID" yang diminta oleh requirement. Nilai 0 berarti event dari server tanpa balasan. Client mengalokasikan correlation secara monotonic per koneksi. Response membawa correlation yang sama dengan request.

flag headers, opsional
Blok header yang kehadirannya ditentukan flags. Urutannya tetap: trace context bila TRACED, fragment info bila FRAGMENT, metadata block bila METADATA. Urutan tetap membuat parser tidak perlu menebak.

payload, bytes
Sisa frame, dienkode dengan MSE sesuai section 143.

Header minimum adalah 4 byte, yaitu version, flags, opcode satu byte, correlation satu byte.

Tentang payload length. Requirement meminta payload length di envelope, dan Migo memenuhinya dari transport, bukan dengan mengulang informasi yang sama di dalam frame:

Pada WebSocket, panjang berasal dari WebSocket message boundary
Pada QUIC datagram, panjang berasal dari datagram boundary
Pada QUIC stream dan TCP, panjang berasal dari length prefix u32 big-endian
Untuk kasus di mana sebuah frame perlu menyatakan panjangnya sendiri, misalnya saat frame disimpan di redelivery buffer, di arsip, atau di dalam BATCH, panjang dibawa oleh pembungkusnya: BATCH membawa varint len per sub-frame, dan penyimpanan memakai length prefix yang sama

Menempatkan payload length kedua di dalam setiap frame berarti membayar 2 sampai 3 byte per frame untuk informasi yang sudah dimiliki penerima, dan menciptakan dua sumber kebenaran yang dapat bertentangan. Pada protokol yang menargetkan typing event 12 byte, itu bukan trade-off yang menguntungkan.


140. FRAME FLAGS

STATUS: BUILT. Kode ada di server/crates/migo-wire/src/flags.rs.

Bit dan artinya:

0x01 COMPRESSED
Payload dikompresi dengan deflate-raw. Hanya diset bila benar-benar mengecil, lihat section 155.

0x02 TRACED
Sebelum payload ada 16 byte trace id dan 8 byte span id, total 24 byte.

0x04 BATCH
Payload berisi varint count lalu count kali pasangan varint len dan sub-frame. Lihat section 154.

0x08 ERROR
Payload adalah struct Error, bukan response normal dari opcode tersebut.

0x10 ACK_REQUIRED
Penerima WAJIB mengirim ACK berisi watermark. Lihat section 151.

0x20 FRAGMENT
Sebelum payload ada varint index dan varint total. Payload adalah potongan dari frame logis yang lebih besar.

0x40 METADATA
STATUS: SPEC. Sebelum payload ada metadata block sesuai section 141. Pada MWP/1 versi sekarang bit ini masih reserved dan WAJIB bernilai 0 sampai section 141 diimplementasikan. Di meta.json bit ini masih bernama RESERVED_6, dan nama METADATA baru dipakai pada saat implementasi, bersamaan dengan perubahan meta.json, migo-wire, dan conformance vector.

0x80 FLAGS_EXT
Ada byte flags kedua. Direservasi untuk MWP/2. Pada MWP/1 WAJIB bernilai 0.

Bit flag yang tidak dikenal WAJIB ditolak dengan ERROR UNSUPPORTED_FLAG. Ini disengaja. Mengabaikan flag yang tidak dikenal berarti pengirim tidak dapat membedakan penerima yang mengerti dari penerima yang berpura-pura mengerti, dan setelah itu protocol tidak dapat lagi diperluas dengan aman.


141. OPTIONAL METADATA BLOCK

STATUS: SPEC. Belum ada di migo-wire. Bit 0x40 masih reserved sampai bagian ini diimplementasikan dan test vector-nya ditambahkan.

Requirement meminta sequence number dan timestamp sebagai bagian dari envelope. Migo menyediakannya dalam dua tingkat.

Tingkat pertama, di dalam payload, dan ini sudah berjalan sekarang. Sequence dan timestamp diletakkan di tempat yang memberi makna:

MessageAccepted membawa seq dan created_at
MessageEvent membawa seq dan created_at
Ack membawa frame_seq
Welcome membawa server_time
SyncResponse membawa from_seq dan to_seq

Menempatkannya di payload berarti frame yang tidak memerlukannya, misalnya PING dan typing event, tidak membayar byte untuk field yang tidak dipakai.

Tingkat kedua, metadata block pada level frame, untuk kebutuhan yang bersifat lintas opcode. Layout, semuanya varint:

frame_seq, varint
Nomor urut frame per arah per session. Naik satu untuk setiap frame yang dikirim. Dipakai untuk ACK dan resume tanpa membaca payload.

sent_at_delta, varint
Selisih milidetik dari server_time yang diberikan pada Welcome untuk arah server ke client, atau dari waktu HELLO untuk arah client ke server. Delta dipilih daripada timestamp absolut karena delta pada sesi yang berjalan hanya butuh 2 sampai 3 byte, sedangkan timestamp absolut butuh 6 byte.

payload_len, varint, opsional di dalam block
Hadir hanya bila frame perlu menyatakan panjangnya sendiri, misalnya saat frame disimpan atau diteruskan tanpa pembungkus.

Aturan:

Metadata block hanya boleh dikirim bila kedua sisi menegosiasikan feature bit yang bersangkutan
Untuk frame berkelas Droppable, metadata block SEBAIKNYA tidak dikirim, karena frame yang boleh hilang tidak perlu dilacak
Ketika bagian ini diimplementasikan, section 140, section 151, dan test vector di shared/protocol/vectors WAJIB diperbarui bersamaan, dan status di section 177 diubah dari SPEC menjadi BUILT


142. BASE TYPES AND VARINT

STATUS: BUILT.

Semua integer tanpa tanda dienkode sebagai LEB128 unsigned varint. Semua integer bertanda dienkode zig-zag lalu LEB128.

Tabel tipe:

bool
1 byte, hanya 0 atau 1. Nilai lain adalah decode error.

u8, u16, u32, u64
LEB128 unsigned varint, maksimum 10 byte.

i32, i64
zig-zag lalu LEB128.

f32, f64
Fixed 4 atau 8 byte little-endian.

string
varint len lalu UTF-8. UTF-8 tidak valid adalah decode error.

bytes
varint len lalu raw byte.

id
Fixed 16 byte, ULID atau UUIDv7, tanpa length prefix karena panjangnya sudah tetap.

timestamp
varint milidetik sejak Migo epoch 2024-01-01T00:00:00Z, yaitu 1704067200000 pada Unix epoch.

duration_ms
varint milidetik.

enum
varint discriminant. Nilai yang tidak dikenal didecode menjadi variant Unknown, bukan error.

list of T
varint count lalu item.

map of string ke T
varint count lalu pasangan string dan T, kunci diurutkan supaya encoding bersifat deterministik.

struct
Nested, mengikuti aturan MSE.

bitmask64
varint dari u64.

Dua keputusan yang perlu dijelaskan:

Migo epoch dipakai daripada Unix epoch karena menghemat satu byte per timestamp selama sekitar 40 tahun ke depan. Pada jutaan pesan per menit satu byte itu nyata.

Setiap enum WAJIB memiliki variant Unknown bernilai 0. Ini yang membuat server baru dapat mengirim variant baru ke client lama tanpa memecahkan decoder.

Batas keras yang diperiksa codec, bukan oleh caller:

MAX_FRAME_BYTES 262144
MAX_STRING_BYTES 65536
MAX_BYTES_LEN 131072
MAX_LIST_ITEMS 4096
MAX_MAP_ITEMS 1024
MAX_NESTING_DEPTH 16
MAX_BATCH_ITEMS 256
MAX_VARINT_BYTES 10

Setiap batas diperiksa sebelum alokasi. Decoder yang membaca varint count bernilai dua miliar lalu memanggil alokasi sebesar itu adalah remote out-of-memory. Decoder Migo mengembalikan error LimitExceeded. Ini hal pertama yang diserang fuzzer.


143. MSE STRUCT ENCODING

STATUS: BUILT.

MSE adalah Migo Struct Encoding. Sebuah struct dienkode sebagai required field secara posisional, lalu bagian optional:

struct = required_field* , varint optional_count , optional_entry*
optional_entry = varint field_id , varint byte_len , bytes

Konsekuensinya, dan semuanya disengaja:

Required field tidak membayar overhead framing sama sekali. Tidak ada tag, tidak ada length.
Optional field membayar sekitar 2 byte overhead dan boleh ditambahkan kapan saja. Penerima lama melewati field_id yang tidak dikenal berdasarkan byte_len. Inilah seluruh cerita forward compatibility, dan ia bekerja tanpa negosiasi apa pun.
Required field bersifat frozen selama umur satu protocol version. Mengubahnya berarti versi mayor baru.
field_id pada optional entry tidak boleh dipakai ulang untuk arti yang berbeda, walaupun field lamanya sudah dihapus.

Mengapa bukan Protobuf. Requirement menyebut Protobuf atau schema binary yang mature, dan Migo memilih MSE dengan IDL yang di-commit sebagai schema binary matang tersebut. Alasannya:

Protobuf membayar overhead tag pada setiap field, termasuk field yang selalu ada. MSE membayarnya hanya pada optional field. Untuk MessageEvent yang mayoritas field-nya wajib, selisihnya belasan byte per pesan
Protobuf menarik compiler tambahan ke setiap toolchain, termasuk toolchain Android dan web. MSE memakai satu generator Node yang hasilnya di-commit, sehingga build server tidak memerlukan langkah generate
Kontrol byte yang tepat diperlukan untuk memenuhi budget seperti typing event 12 byte. Dengan MSE, layout byte terlihat langsung dari IDL
Codec MSE berukuran kecil dan seluruh jalur decode dapat difuzz secara menyeluruh
Properti evolusi yang dibutuhkan Migo, yaitu skip berdasarkan panjang dan enum Unknown, sudah didapatkan tanpa membawa seluruh permukaan Protobuf

Keputusan ini tercatat di ADR-0002. Bila kelak Protobuf dipilih, itu adalah perubahan MWP/2 dengan masa dual-speak, bukan perubahan diam-diam.


144. IDL AND CODE GENERATION

STATUS: BUILT.

IDL berada di shared/protocol/schema dan merupakan sumber kebenaran tunggal:

meta.json
Protocol version, limits, flag bit, feature bit, delivery class, auth level, dan Migo epoch.

opcodes.json
Registry packet type beserta arah, payload struct, response struct, auth level, rate limit cost, dan delivery class.

structs.json
Definisi struct beserta required field dan optional field dengan field_id.

enums.json
Definisi enum beserta nilai numeriknya.

errors.json
Registry error code beserta symbol, HTTP status, dan class.

Generator ada di tools/protocol-codegen dan menghasilkan:

server/crates/migo-protocol/src/generated.rs
packages/protocol untuk TypeScript

Aturan wajib:

Hasil generate ikut di-commit sesuai ADR-0010. Build server dan build client tidak boleh memerlukan Node
Hasil generate yang tidak sinkron dengan IDL adalah kegagalan build melalui make protocol-check, bukan peringatan
Menambah opcode tanpa mencantumkan rate limit cost adalah error generator, bukan default diam-diam
Payload struct dan response struct yang disebut di opcodes.json WAJIB ada di structs.json
Test vector biner di shared/protocol/vectors WAJIB menghasilkan byte identik pada Rust dan TypeScript. Perbedaan satu byte antara dua bahasa adalah bug yang hanya muncul di production bila tidak diuji di sini


145. PACKET TYPE REGISTRY

Range opcode:

1 sampai 15 control
16 sampai 31 auth dan keys
32 sampai 63 messaging
64 sampai 79 presence
80 sampai 111 rooms
112 sampai 127 social
128 sampai 143 media
144 sampai 159 notifications
160 sampai 175 economy
176 sampai 191 games dan bots
192 sampai 207 moderation
208 sampai 223 federation
224 sampai 239 calls
240 sampai 255 reserved

Konvensi penamaan. Nama opcode memakai SCREAMING_SNAKE_CASE. Response TIDAK memiliki opcode sendiri: response dibawa pada correlation dari request-nya dan diacu dengan nama struct dalam PascalCase, misalnya MESSAGE_SEND dijawab MessageAccepted, SYNC dijawab SyncResponse, dan SUBSCRIBE dijawab SubscribeResponse. Karena itu satu nomor opcode mencakup satu request beserta response-nya, dan jumlah opcode tidak membengkak dua kali.

Opcode yang sudah ada di schema. STATUS: SCHEMA. Format setiap baris: nomor, nama, arah, auth level, cost, delivery class.

1 HELLO, client ke server, None, 5, Critical
2 PING, dua arah, None, 1, Critical
3 ACK, client ke server, None, 0, Critical
4 ERROR, server ke client, None, 0, Critical
5 RECONNECT_HINT, server ke client, None, 0, Critical
6 AUTHENTICATE, client ke server, None, 10, Critical
7 SUBSCRIBE, client ke server, User, 2, Critical
8 UNSUBSCRIBE, client ke server, User, 1, Critical
16 KEY_PUBLISH, client ke server, User, 20, Critical
17 KEY_BUNDLE_FETCH, client ke server, User, 5, Critical
32 MESSAGE_SEND, client ke server, User, 1, Critical
33 MESSAGE_EVENT, server ke client, User, 0, Critical
34 MESSAGE_RECEIPT, dua arah, User, 1, Critical
35 MESSAGE_DELETE, client ke server, User, 2, Critical
36 SYNC, client ke server, User, 3, Critical
37 CONVERSATION_LIST, client ke server, User, 3, Critical
38 CONVERSATION_CREATE, client ke server, User, 10, Critical
39 TYPING, dua arah, User, 1, Coalescable
64 PRESENCE_SET, client ke server, User, 1, Coalescable
65 PRESENCE_EVENT, server ke client, User, 0, Coalescable
80 ROOM_JOIN, client ke server, User, 20, Critical
81 ROOM_LEAVE, client ke server, User, 5, Critical
82 ROOM_LIST, client ke server, User, 5, Critical
85 ROOM_CREATE, client ke server, User, 20, Critical
86 ROOM_ROSTER, client ke server, User, 3, Critical
87 ROOM_ROLE_SET, client ke server, User, 5, Critical
88 ROOM_UPDATE, client ke server, User, 5, Critical
89 ROOM_ARCHIVE, client ke server, User, 5, Critical
83 ROOM_MEMBER_EVENT, server ke client, User, 0, Coalescable
84 ROOM_STATE_EVENT, server ke client, User, 0, Coalescable
112 PROFILE_FETCH, client ke server, User, 3, Critical
144 NOTIFICATION_EVENT, server ke client, User, 0, Droppable
176 GAME_ACTION, client ke server, User, 2, Critical
177 GAME_EVENT, server ke client, User, 0, Critical
183 GAME_START, client ke server, User, 5, Critical
184 GAME_VIEW, client ke server, User, 2, Critical
185 GAME_ABANDON, client ke server, User, 2, Critical
186 GAME_CATALOGUE, client ke server, User, 1, Critical

Opcode yang direncanakan. STATUS: BUILT untuk seluruh range yang tercantum di atas, termasuk call 224 sampai 238. STATUS: SCHEMA untuk metadata block section 141 dan flag bit 0x40 yang belum masuk registri. STATUS: SPEC untuk SFU group call penuh yang membutuhkan deployment terpisah. Setiap opcode ditambahkan ke opcodes.json bersamaan dengan implementasi handler-nya, sesuai aturan alokasi section 146.

Messaging:

40 MESSAGE_EDIT, client ke server, User, 2, Critical
41 REACTION_SET, client ke server, User, 1, Critical
42 REACTION_EVENT, server ke client, User, 0, Coalescable

Social:

111 PROFILE_UPDATE, client ke server, User, 3, Critical
113 FRIEND_REQUEST, client ke server, User, 10, Critical
114 FRIEND_RESPOND, client ke server, User, 5, Critical
115 FRIEND_EVENT, server ke client, User, 0, Critical
116 BLOCK_SET, client ke server, User, 5, Critical
117 RELATIONSHIP_LIST, client ke server, User, 3, Critical
118 SUGGESTIONS, client ke server, User, 3, Critical
119 SEARCH, client ke server, User, 3, Critical

Media:

128 MEDIA_UPLOAD_BEGIN, client ke server, User, 10, Critical
129 MEDIA_UPLOAD_STATUS, client ke server, User, 2, Critical
130 MEDIA_UPLOAD_COMMIT, client ke server, User, 5, Critical
131 MEDIA_UPLOAD_ABORT, client ke server, User, 1, Critical
132 MEDIA_FETCH_URL, client ke server, User, 3, Critical
133 MEDIA_STATE_EVENT, server ke client, User, 0, Coalescable

Notifications:

145 NOTIFICATION_ACK, client ke server, User, 1, Critical
146 NOTIFICATION_LIST, client ke server, User, 3, Critical

Economy:

160 GIFT_SEND, client ke server, User, 20, Critical
161 BALANCE_FETCH, client ke server, User, 3, Critical
162 ECONOMY_EVENT, server ke client, User, 0, Critical
163 GIFT_CATALOGUE, client ke server, User, 1, Critical
164 LEDGER_HISTORY, client ke server, User, 3, Critical
165 PROGRESSION, client ke server, User, 2, Critical
166 BADGES, client ke server, User, 2, Critical
167 LEADERBOARD, client ke server, User, 5, Critical

Bots:

178 BOT_COMMAND, client ke server, User, 2, Critical
179 BOT_EVENT, server ke client, User, 0, Critical
180 BOT_REGISTER, client ke server, Bot, 20, Critical

Moderation:

192 REPORT_CREATE, client ke server, User, 20, Critical
193 MODERATION_ACTION, client ke server, User, 10, Critical
194 MODERATION_EVENT, server ke client, User, 0, Critical

Federation. Semua auth level Server dan hanya diterima pada listener mesh:

208 FED_HELLO, dua arah, Server, 5, Critical
209 FED_AUTH, dua arah, Server, 5, Critical
210 FED_PING, dua arah, Server, 1, Critical
211 FED_FORWARD, dua arah, Server, 1, Critical
212 FED_ACK, dua arah, Server, 0, Critical
213 FED_ROOM_SUBSCRIBE, dua arah, Server, 2, Critical
214 FED_ROOM_EVENT, dua arah, Server, 0, Critical
215 FED_PRESENCE_DIGEST, dua arah, Server, 0, Coalescable
216 FED_KEY_ROTATE, dua arah, Server, 5, Critical
217 FED_HEALTH, dua arah, Server, 1, Critical
218 FED_SHARD_MAP, dua arah, Server, 2, Critical
219 FED_ERROR, dua arah, Server, 0, Critical
220 FED_CALL_RELAY, dua arah, Server, 1, Critical
221 FED_DIRECTORY, dua arah, Server, 2, Critical

Calls:

224 CALL_INVITE, client ke server, User, 20, Critical
225 CALL_INVITE_EVENT, server ke client, User, 0, Critical
226 CALL_ANSWER, client ke server, User, 5, Critical
227 CALL_DECLINE, client ke server, User, 5, Critical
228 CALL_CANCEL, client ke server, User, 5, Critical
229 CALL_END, dua arah, User, 2, Critical
230 CALL_SDP, dua arah, User, 3, Critical
231 CALL_ICE, dua arah, User, 1, Critical
232 CALL_STATE_EVENT, server ke client, User, 0, Critical
233 CALL_RENEGOTIATE, dua arah, User, 3, Critical
234 CALL_KEY_UPDATE, dua arah, User, 3, Critical
235 CALL_STATS, client ke server, User, 1, Droppable
236 CALL_TURN_FETCH, client ke server, User, 10, Critical
237 CALL_SFU_JOIN, client ke server, User, 20, Critical
238 CALL_SFU_EVENT, server ke client, User, 0, Coalescable

Catatan tentang opcode call dan media yang bernilai di atas 127, sehingga memakai dua byte varint. Ini pilihan sadar. Call signaling hanya beberapa frame per panggilan dan media control hanya beberapa frame per upload, sehingga satu byte tambahan tidak berpengaruh. Range di bawah 128 dijaga untuk opcode berfrekuensi tinggi seperti MESSAGE_EVENT, TYPING, dan PRESENCE_EVENT.

Enum di schema. STATUS: SCHEMA. Setiap enum diserialisasi sebagai varint dan setiap enum WAJIB memiliki varian Unknown bernilai 0, sehingga penerima lama yang membaca nilai baru mendapatkan Unknown alih-alih gagal mendekode. Menambah varian baru di akhir adalah perubahan yang backward compatible; mengubah nomor varian yang sudah dipakai TIDAK BOLEH dilakukan tanpa menaikkan versi protokol.

Platform: Unknown, Web, Android, Ios, Desktop, Bot, LoadTest
BandwidthMode: Unknown, Auto, Normal, LowData, UltraLowData
PresenceState: Unknown, Offline, Online, Away, Busy, Invisible
MessageKind: Unknown, Text, Media, System, Game, Gift, Sticker, Voice, KeyExchange
ConversationKind: Unknown, Direct, Group, Room
EncryptionMode: Unknown, None, Transport, EndToEnd
ReceiptKind: Unknown, Delivered, Read
TypingState: Unknown, Start, Stop
SyncStatus: Unknown, Ok, Truncated
TopicKind: Unknown, Conversation, Room, User, Game
RoomKind: Unknown, Public, Managed
RoomRole: Unknown, Member, Helper, Moderator, Admin, Manager, Owner
NotificationKind: Unknown, Message, Mention, Reply, FriendRequest, Gift, LevelUp, Achievement, RoomInvite, RoomAnnouncement, Event, GameChallenge
RelationshipKind: Unknown, Friend, PendingOutgoing, PendingIncoming, Follow, Block, Favorite
CloseReason: Unknown, ClientRequest, ServerShutdown, NodeDraining, SessionLagging, ResumeRequired, AuthExpired, Rebalance, ProtocolViolation

Catatan tentang EncryptionMode. Nilai None bernilai 1, bukan 0, karena 0 dipakai Unknown. Perbedaan None dan Transport bersifat penting: None berarti tidak ada enkripsi tambahan di atas transport dan hanya sah untuk system message, Transport berarti hanya dilindungi TLS sehingga server dapat membacanya, dan EndToEnd berarti server hanya menyimpan ciphertext. Client TIDAK BOLEH menampilkan label terenkripsi ujung ke ujung kecuali nilainya EndToEnd.

Catatan tentang RoomRole. Urutan nilai bersifat hierarkis dan pemeriksaan izin SEBAIKNYA memakai perbandingan lebih besar atau sama dengan, bukan daftar peran yang di-hardcode, agar penambahan peran baru tidak melewatkan pemeriksaan yang sudah ada.

Auth level:

None
Hanya untuk handshake, yaitu HELLO, AUTHENTICATE, PING, dan resume.

User
Memerlukan session terautentikasi.

Bot
Memerlukan bot token dan berjalan dengan izin terbatas.

Server
Hanya diterima pada listener mesh internal. Frame dengan auth level Server yang datang dari socket client WAJIB ditolak dan dicatat sebagai insiden keamanan.


146. RESERVED RANGES AND FUTURE PACKETS

STATUS: SPEC. Range masih berupa kesepakatan dokumen. Penegakan range dilakukan di dispatcher gateway yang belum ditulis.

Range 240 sampai 255 direservasi dan WAJIB tidak dipakai sampai ada keputusan tertulis.

Aturan alokasi opcode baru:

Opcode baru dialokasikan di dalam range domainnya, bukan di celah terdekat
Opcode yang pernah dipakai lalu dihentikan TIDAK BOLEH dipakai ulang untuk arti berbeda. Nomornya ditandai deprecated di opcodes.json dan dibiarkan kosong
Opcode berfrekuensi tinggi baru harus mencari slot di bawah 128. Bila range domainnya sudah habis di bawah 128, pilihannya adalah menerima dua byte, bukan mencuri slot domain lain
Setiap opcode baru WAJIB membawa delivery class, auth level, dan rate limit cost sejak commit pertama


147. SCHEMA VERSIONING AND BACKWARD COMPATIBILITY

STATUS: BUILT untuk aturan decoder.

Tiga versi yang berbeda dan tidak boleh dicampur:

Frame version
Byte pertama setiap frame. Bernilai 1 untuk MWP/1. Berubah hanya bila framing berubah.

Protocol version pada HELLO
Field protocol_version berupa u32. Dipakai untuk menyampaikan revisi semantik di dalam frame version yang sama, misalnya penambahan opcode dalam jumlah besar.

Schema revision
Nilai di meta.json yang naik setiap kali IDL berubah. Dipakai untuk observability dan untuk memastikan hasil generate sinkron, bukan untuk gating runtime.

Perubahan yang aman di dalam MWP/1:

Menambah opcode
Menambah optional field dengan field_id baru
Menambah enum variant
Menambah feature bit
Menambah error code
Menambah delivery class baru pada opcode baru

Perubahan yang memerlukan MWP/2:

Mengubah, menghapus, atau mengurutkan ulang required field
Mengubah tipe sebuah field
Mengubah arti opcode yang sudah dipakai
Mengubah encoding primitive
Mengubah arti flag bit

Kewajiban decoder, dan setiap butir ini memiliki test:

Optional field dengan field_id tidak dikenal dilewati berdasarkan byte_len tanpa error
Enum value tidak dikenal menjadi Unknown
Trailing byte setelah struct selesai adalah error, sehingga field yang salah panjang tidak lolos diam-diam
Opcode tidak dikenal dijawab ERROR UNKNOWN_OPCODE, sesi berlanjut
Opcode dikenal tetapi salah state dijawab ERROR UNEXPECTED_OPCODE
Flag bit tidak dikenal dijawab ERROR UNSUPPORTED_FLAG lalu koneksi ditutup
Frame version tidak didukung dijawab ERROR PROTOCOL_VERSION_UNSUPPORTED lalu koneksi ditutup
Frame melebihi MAX_FRAME_BYTES dijawab ERROR FRAME_TOO_LARGE sebelum buffer dialokasikan

Kewajiban encoder:

TIDAK BOLEH mengirim optional field yang bernilai default atau kosong. Field yang tidak berubah tidak dikirim
TIDAK BOLEH mengirim frame untuk fitur yang tidak dinegosiasikan
TIDAK BOLEH mengirim flag reserved dengan nilai bukan nol


148. FEATURE NEGOTIATION BITS

STATUS: SCHEMA untuk bit 0 sampai 15 di meta.json. STATUS: SPEC untuk bit 16 sampai 20 dan untuk logika negosiasi di gateway.

Bit 16 sampai 20 yang masih SPEC adalah VOICE_NOTE, CALLS, GROUP_CALL, FEDERATION, dan RICH_PRESENCE; setiap bit menyalakan kelompok opcode yang bersangkutan (MEDIA untuk VOICE_NOTE, CALLS dan GROUP_CALL untuk range 224 sampai 238, FEDERATION untuk range 208 sampai 223, dan RICH_PRESENCE untuk status kustom).

Mekanisme dan daftar bit ada di section 72. Bagian ini menetapkan aturan protokolnya.

HELLO membawa features berupa bitmask64 milik client. Welcome membawa features milik server. Session memakai irisan keduanya, dan irisan itu bersifat tetap selama sesi berlangsung.

Aturan:

Fitur tidak dapat dinyalakan di tengah sesi. Bila server mengaktifkan fitur baru, client mendapatkannya pada koneksi berikutnya. Ini menghindari state negosiasi yang berubah-ubah dan sulit diuji
Server yang menerima permintaan untuk fitur yang tidak dinegosiasikan menjawab ERROR FEATURE_NOT_NEGOTIATED
Server yang fiturnya dimatikan oleh kill switch tidak mengiklankan bit tersebut, dan permintaan terkait dijawab ERROR FEATURE_DISABLED
Bit yang tidak dikenal diabaikan, karena bitmask fitur bersifat aditif dan tidak mengubah cara byte dibaca


149. CONNECTION LIFECYCLE

STATUS: SCHEMA.

Alur normal:

client                                            gateway
  |  -- HELLO { protocol_version, client, features, locale,
  |             bandwidth_mode, [access_token], [device_id],
  |             [resume] } -->
  |
  |  <-- Welcome { session_id, node, features, server_time,
  |                limits, [resumed], [resume_from_seq],
  |                [authenticated_user] } --
  |        atau ERROR, atau RECONNECT_HINT
  |
  |  -- AUTHENTICATE { access_token, device_id } -->   bila belum di HELLO
  |  <-- AUTHENTICATED { user, device, capabilities } --
  |
  |  -- SUBSCRIBE { topics[] } -->
  |  <-- SubscribeResponse { accepted, rejected } --
  |
  |  <-- event ... --
  |  -- PING -->    <-- Pong --
  |  -- ACK { frame_seq } -->

Aturan state machine:

Sebelum Welcome, hanya HELLO yang diterima. Frame lain dijawab UNEXPECTED_OPCODE
Sebelum AUTHENTICATED, hanya HELLO, AUTHENTICATE, PING, dan resume yang diterima
HELLO dua kali pada satu koneksi adalah protocol violation
Token boleh dikirim di HELLO untuk menghemat satu round trip. Bila token tidak valid, jawabannya ERROR pada HELLO, bukan Welcome lalu error
Batas MAX_SUBSCRIPTIONS adalah 512 topic per session. Melebihi batas dijawab TOO_MANY_SUBSCRIPTIONS
Heartbeat interval berasal dari DEFAULT_HEARTBEAT_MS, yaitu 30000 ms, dan diberikan server melalui struct Limits di Welcome. Melewatkan dua interval berarti socket ditutup
Server dapat menutup koneksi dengan RECONNECT_HINT berisi CloseReason. Nilai CloseReason yang ada: ClientRequest, ServerShutdown, NodeDraining, SessionLagging, ResumeRequired, AuthExpired, Rebalance, ProtocolViolation
Shutdown yang rapi mengirim RECONNECT_HINT lebih dulu dengan after_ms yang diacak, lalu menutup socket setelah outbound queue kosong atau setelah deadline


150. RECONNECT, BACKOFF, AND RESUME

STATUS: SCHEMA.

Backoff:

Basis delay 1s, 2s, 4s, 8s, 16s, 30s
Delay yang dipakai adalah nilai acak seragam antara 0 dan basis delay. Ini full jitter
Setelah 30s, delay bertahan di 30s dengan jitter
Attempt counter dicatat dan dilaporkan sebagai metric
Bila server memberi after_ms melalui RECONNECT_HINT, nilai itu menang atas backoff lokal
Perpindahan jaringan yang terdeteksi sistem operasi memicu percobaan segera tanpa menunggu backoff, karena penyebabnya sudah diketahui

Resume:

HELLO membawa resume berisi session_id dan last_frame_seq
Gateway menyimpan ring buffer frame berkelas Critical per session, kapasitas RESUME_BUFFER_FRAMES yaitu 512 frame, dengan jendela RESUME_WINDOW_MS yaitu 120000 ms
Bila buffer masih mencakup last_frame_seq, gateway membalas Welcome dengan resumed bernilai true dan resume_from_seq, lalu mengirim ulang frame yang belum diakui. Terowongan kereta bawah tanah berbiaya beberapa ratus byte, bukan resync penuh
Bila buffer tidak lagi mencakupnya, jawabannya ERROR RESUME_REQUIRED dan client turun ke cursor sync per conversation sesuai section 158
Resume TIDAK BOLEH mengirim ulang frame berkelas Droppable. Frame yang boleh hilang tidak perlu diulang
Frame yang dikirim ulang membawa id yang sama seperti aslinya, sehingga dedup di client membuat replay tidak berbahaya


151. ACK, RETRY, AND DELIVERY CLASS

STATUS: SCHEMA.

Tiga delivery class dan perlakuannya:

Critical
Tidak pernah dibuang. Bila outbound queue penuh melewati LAGGING_DEADLINE_MS yaitu 5000 ms, session ditutup dengan CloseReason SessionLagging dan ERROR SESSION_LAGGING, lalu client melakukan resume. Membuang pesan diam-diam jauh lebih buruk daripada memaksa reconnect.

Coalescable
Nilai terbaru per coalescing key menggantikan nilai lama di dalam queue. Presence, typing, dan counter masuk kelas ini. Kehilangan nilai lama tidak berarti apa-apa karena nilai baru sudah menggantikannya.

Droppable
Boleh dibuang tanpa suara saat tekanan tinggi, tetapi WAJIB dihitung sebagai metric. Frame yang dibuang tanpa terlihat adalah cara termudah kehilangan bug selama berbulan-bulan.

ACK:

Frame yang diberi flag ACK_REQUIRED ditahan di redelivery buffer sampai client mengirim ACK
ACK membawa watermark kumulatif, sehingga satu ACK melunasi ratusan frame. Ukurannya maksimum 10 byte
Client SEBAIKNYA mengirim ACK dengan batching, misalnya setiap 200 ms atau setiap 32 frame, mana yang lebih dulu
Server TIDAK BOLEH menunggu ACK per frame sebelum mengirim frame berikutnya. ACK bersifat kumulatif dan asinkron

Retry:

Retry client memakai message_id yang sama, sehingga bersifat idempotent
Retry memakai exponential backoff dengan jitter, tidak pernah retry rapat
Error dengan class RateLimit dan Server boleh diretry. Error dengan class Protocol, Auth, Permission, dan Validation TIDAK BOLEH diretry otomatis
Error dengan retry_after_ms WAJIB dihormati. Mengabaikannya berarti memperparah kondisi overload yang menyebabkan error tersebut
Batas percobaan per item queue ditetapkan, dan setelah batas tercapai item ditandai gagal dan ditampilkan kepada user, bukan diulang selamanya


152. SEQUENCE NUMBER SPACES

STATUS: BUILT untuk conversation sequence di migo-store. STATUS: SPEC untuk frame sequence, federation link sequence, dan ratchet counter.

Empat ruang sequence yang terpisah. Mencampurnya adalah sumber bug yang mahal, jadi keempatnya diberi nama berbeda di kode dan di dokumen.

conversation seq, u64
Diberikan server per conversation. Monotonic, tanpa lubang, tidak pernah dipakai ulang. Menentukan urutan pesan dan menjadi cursor untuk sync. Lihat section 67.

frame_seq, u64
Per session, per arah. Dipakai untuk ACK dan resume. Tidak berhubungan dengan urutan pesan.

federation link sequence, u64
Per pasangan node, per arah. Dipakai untuk replay protection. Packet dengan sequence tidak lebih besar dari yang terakhir diterima ditolak.

ratchet counter
Di dalam cryptographic envelope. Dipakai oleh Double Ratchet dan sender-key ratchet. Server tidak dapat membacanya.

Aturan:

Nama field di kode WAJIB memakai nama di atas, bukan seq secara umum
Gap pada conversation seq berarti client meminta range yang hilang, dijawab dengan SEQUENCE_GAP bila range sudah tidak tersedia
Gap pada frame_seq berarti resume, bukan permintaan pesan
Gap pada federation link sequence berarti dugaan replay atau kehilangan link, dan link direset


153. DEDUPLICATION AND IDEMPOTENCY

STATUS: BUILT untuk deduplikasi client_msg_id di backend in-memory migo-store. STATUS: SPEC untuk idempotency di seluruh surface lain.

Aturan dan layernya ada di section 68. Bagian ini menetapkan aturan protokolnya.

Setiap operasi yang mengubah state WAJIB memiliki idempotency key yang dibuat client:

Message memakai message_id
Reaction memakai kombinasi message_id, user_id, dan emoji, sehingga reaksi yang sama tidak dihitung dua kali
Gift dan transaksi ekonomi memakai idempotency key eksplisit, dan transaksi ganda pada uang virtual adalah bug paling merugikan yang bisa terjadi
Media upload memakai upload_id
Friend request memakai kombinasi pasangan account
Call invite memakai call_id

Perilaku server:

Permintaan ulang dengan key yang sama dan payload sama menghasilkan jawaban yang sama seperti permintaan pertama, tanpa efek samping baru
Permintaan ulang dengan key yang sama tetapi payload berbeda dijawab ERROR IDEMPOTENCY_MISMATCH. Menerima keduanya berarti key tidak berguna
Idempotency record disimpan minimal selama jendela retry yang mungkin, dan untuk transaksi ekonomi disimpan permanen di ledger


154. BATCHING AND COALESCING

STATUS: SCHEMA.

Batching:

Frame dengan flag BATCH membawa varint count lalu count kali pasangan varint len dan sub-frame
Batas MAX_BATCH_ITEMS adalah 256
Linger maksimum BATCH_LINGER_MS yaitu 15 ms. Cukup lama untuk mengumpulkan burst, cukup singkat untuk tidak terasa
Batching mengamortisasi header 4 byte, dan yang lebih penting mengamortisasi satu WebSocket message, satu TLS record, dan satu radio wake-up
Sub-frame di dalam BATCH TIDAK BOLEH berisi BATCH lagi. Nesting batch tidak menambah manfaat dan menambah permukaan serangan

Coalescing:

Berlaku hanya untuk delivery class Coalescable
Coalescing key ditentukan per opcode. Untuk presence, key-nya user_id. Untuk typing, key-nya kombinasi conversation_id dan user_id. Untuk room counter, key-nya room_id
Di dalam satu linger window, hanya nilai terbaru per key yang bertahan
Room dengan 800 orang yang berubah presence menghasilkan satu update teragregasi, bukan 800 frame

Kedua mekanisme ini terpisah dan sering dipakai bersama: coalescing mengurangi jumlah event, batching mengurangi jumlah pengiriman.


155. COMPRESSION POLICY

STATUS: BUILT untuk aturan keputusan.

Payload dikompresi hanya bila ketiga syarat berikut terpenuhi:

Kedua sisi menegosiasikan feature bit COMPRESSION
Ukuran payload minimal COMPRESS_MIN_BYTES yaitu 512 byte
Hasil kompresi minimal COMPRESS_MIN_GAIN_PERCENT yaitu 10 persen lebih kecil

Bila salah satu tidak terpenuhi, payload dikirim mentah dan flag COMPRESSED tidak diset.

Algoritma yang dipakai adalah deflate-raw. Dipilih karena browser mengimplementasikannya secara native melalui CompressionStream, sehingga web client tidak membayar byte bundle sama sekali.

Aturan tambahan:

Mengompresi pesan chat 60 byte membuatnya lebih besar dan membakar baterai di kedua ujung
Payload yang sudah terenkripsi E2E hampir tidak dapat dikompresi. Client SEBAIKNYA melewatkan percobaan kompresi untuk field envelope
Media TIDAK BOLEH dikompresi lagi di layer protocol, karena sudah terkompresi oleh codec-nya
Kompresi TIDAK BOLEH diterapkan pada gabungan payload dari beberapa pengirim di dalam satu konteks, karena itu membuka kelas serangan compression oracle. Setiap payload dikompresi sendiri
permessage-deflate di WebSocket WAJIB dimatikan agar tidak ada dua lapis keputusan kompresi


156. COMPACT IDENTIFIERS AND DELTA UPDATES

STATUS: BUILT untuk Id 16 byte di migo-core dan encoding-nya di migo-wire. STATUS: SPEC untuk delta update per surface.

Compact identifier:

Semua id adalah 16 byte biner. UUIDv7 atau ULID, sehingga id juga terurut menurut waktu dan cocok sebagai primary key database
Id TIDAK BOLEH dikirim sebagai string 36 karakter pada realtime path. Selisihnya 20 byte per id, dan satu MessageEvent membawa empat id
Untuk konteks yang berumur panjang, misalnya room besar atau group call, server MENYARANKAN pemakaian short handle. Handle adalah varint kecil yang dipetakan ke id 16 byte, berlaku hanya untuk satu sesi dan satu konteks. Untuk room 800 anggota, mengirim handle 2 byte daripada id 16 byte pada setiap event menghemat 14 byte per event
Pemetaan handle dikirim satu kali saat join, lalu dipakai berulang. Handle TIDAK BOLEH dipakai lintas sesi, karena tidak stabil

Delta updates:

Room state, member list, game state, leaderboard, dan counter dikirim sebagai perubahan
Snapshot penuh hanya dikirim saat join atau ketika client tidak dapat lagi menerapkan delta
Setiap delta membawa nomor epoch atau versi state, sehingga client dapat mendeteksi bahwa ia melewatkan sebuah delta dan meminta snapshot
Delta untuk list memakai operasi eksplisit yaitu add, remove, dan update, bukan mengirim ulang seluruh list

Aturan penutup yang paling penting untuk bandwidth:

TIDAK BOLEH mengirim data yang tidak berubah. Bila state tidak berubah, tidak ada frame. Ini berlaku untuk presence, typing, counter, profile, room setting, dan game state.


157. PAGINATION AND CURSORS

STATUS: BUILT untuk daftar percakapan dan history message di migo-messaging. Struct cursor terpisah ternyata tidak diperlukan: cursor daftar percakapan adalah string opaque pada CONVERSATION_LIST dan struct ConversationListResponse yang sudah ada di shared/protocol/schema, berisi versi, waktu aktivitas terakhir, waktu pembuatan, dan conversation_id, sehingga urutannya total dan client tidak pernah perlu menafsirkan isinya. Cursor history message adalah conversation seq sesuai aturan di bawah. STATUS: SPEC untuk listing lain yang handler-nya belum ditulis.

Semua listing memakai cursor, bukan offset. Offset menghasilkan baris ganda dan baris hilang ketika data berubah di antara dua permintaan.

Aturan:

Batas maksimum halaman ditentukan server. Default-nya 200 dan permintaan yang lebih besar diperkecil, bukan ditolak. Angka ini adalah konfigurasi server, bukan limit di meta.json. Bila kelak client perlu mengetahuinya sebelum meminta, angka itu WAJIB ditambahkan ke struct Limits dan ke meta.json pada perubahan yang sama, bukan ditebak client
Cursor untuk message history adalah conversation seq
Cursor untuk listing lain adalah kombinasi kunci urut dan id, sehingga urutan bersifat total. Urutan yang hanya "hampir terurut" membuat paging menjatuhkan baris
Response membawa penanda more, sehingga client tahu ada halaman berikutnya tanpa harus meminta halaman kosong
History yang sudah tidak tersedia dijawab ERROR HISTORY_TRUNCATED, dan client menampilkan batas riwayat dengan jujur
Penyelesaian ketegangan antara aturan di atas dan section 158: pada SYNC, history yang sudah hilang dijawab SyncResponse dengan status Truncated dan bukan ERROR HISTORY_TRUNCATED, termasuk ketika tidak ada satu baris pun yang dapat dikembalikan. Sebuah error akan menahan pesan yang masih ada dan memaksa client memilih antara menampilkan kegagalan atau menyembunyikan kehilangan, padahal yang dibutuhkan adalah keduanya sekaligus, yaitu pesan yang tersisa beserta pengakuan bahwa ada yang hilang. ERROR HISTORY_TRUNCATED tetap berada di IDL untuk jalur REST history yang belum ditulis, dan bila jalur itu kelak juga memilih status alih-alih error maka symbol tersebut WAJIB dihapus dari errors.json pada perubahan yang sama


158. OFFLINE-FIRST SYNCHRONIZATION

STATUS: BUILT untuk sisi server di migo-messaging, yaitu SYNC maju dan mundur, to_seq untuk mengambil tepat satu gap, status Ok dan Truncated, penanda more, dan read cursor yang bergerak maju saja dengan delivered yang selalu minimal sebesar read. STATUS: SPEC untuk sisi client, yaitu urutan reconnect, outbox, dan penghentian sync ketika aplikasi masuk background.

Urutan sinkronisasi setelah reconnect:

Resume bila memungkinkan. Bila berhasil, tidak ada sync yang diperlukan
Bila resume gagal, ambil CONVERSATION_LIST untuk mendapatkan last_seq dan unread count per conversation
Bandingkan dengan last_seq lokal. Kirim SYNC hanya untuk conversation yang memiliki gap
SYNC membawa conversation_id, have_seq, limit, dan opsional to_seq serta backwards
SyncResponse membawa status Ok atau Truncated, from_seq, to_seq, more, dan daftar MessageEvent
Bila status Truncated, client menampilkan penanda riwayat terpotong dan tidak berpura-pura lengkap

Aturan:

TIDAK BOLEH full resync ketika hanya beberapa pesan yang hilang
TIDAK BOLEH mengambil history conversation yang tidak dibuka user. Yang disinkronkan lebih dulu adalah conversation yang terlihat
Outbox dikirim setelah sync selesai, dengan urutan per conversation dipertahankan
Read cursor bergerak maju saja, dan nilai delivered selalu minimal sebesar nilai read
Sync yang berjalan pada saat aplikasi masuk background dihentikan rapi, bukan dibiarkan menghabiskan baterai


159. ADAPTIVE PRESENCE AND TYPING

STATUS: BUILT untuk tabel cadence di migo-presence, yaitu pengali heartbeat per bandwidth mode, masa hidup presence entry yang diturunkan dari pengali itu supaya client yang menepati interval yang diberikan server tidak pernah berkedip offline, penonaktifan typing pada UltraLowData, dan scope presence yang menyempit menjadi hanya conversation yang terbuka pada LowData maupun UltraLowData. STATUS: SPEC untuk penegakan interval minimum dan penyaringan per session. Interval minimum dihitung dan diumumkan oleh migo-presence tetapi sengaja tidak ditegakkan di sana, karena hanya queue dengan trailing edge dapat menerapkannya tanpa menghilangkan update terakhir, dan queue itu adalah coalescing queue di gateway pada section 154.

Presence:

Presence dikirim hanya ketika berubah
Presence diagregasi per room, bukan per anggota per event
Interval minimum antar update presence untuk satu user ditentukan server dan disesuaikan dengan bandwidth mode
Presence untuk conversation yang tidak terbuka SEBAIKNYA tidak dikirim pada mode LowData, dan TIDAK BOLEH dikirim pada mode UltraLowData
Invisible berarti server tidak mengirim presence untuk user tersebut kepada siapa pun, bukan client yang menyembunyikannya

Typing:

Typing dikirim saat mulai dan saat berhenti, tidak pernah per ketikan
Debounce minimum di client, dan typing stop dikirim otomatis setelah timeout bila user berhenti tanpa mengirim
Typing berkelas Coalescable dengan key kombinasi conversation_id dan user_id
Typing TIDAK BOLEH dikirim pada mode UltraLowData
Typing tidak pernah di-queue saat offline

Pengali frekuensi menurut bandwidth mode:

Normal
Frekuensi penuh

LowData
Presence throttled empat kali lebih lambat, heartbeat lebih panjang, typing tetap ada untuk conversation yang terbuka

UltraLowData
Hanya text dan receipt. Presence hanya untuk conversation yang terbuka. Typing dimatikan. Heartbeat pada interval maksimum

bandwidth_mode dikirim pada HELLO supaya server berhenti mengirim yang tidak akan dirender client. Filter di client menghemat rendering, filter di server menghemat byte.


160. FLOW CONTROL AND BACKPRESSURE

STATUS: SPEC untuk implementasi gateway. Keputusan desain ada di ADR-0008.

Setiap session memiliki outbound queue dengan kapasitas SESSION_QUEUE_CAPACITY yaitu 256 frame.

Ketika queue penuh:

Frame Coalescable menggantikan frame lama dengan key yang sama, sehingga queue tidak bertambah panjang
Frame Droppable dibuang dan dihitung sebagai metric
Frame Critical tidak pernah dibuang. Bila queue tetap penuh melewati LAGGING_DEADLINE_MS yaitu 5000 ms, session ditutup dengan SessionLagging dan client melakukan resume

Aturan:

Server TIDAK BOLEH memakai unbounded channel per session. Satu client lambat dengan queue tak terbatas akan menghabiskan memori seluruh node
Rate limiting bersifat cost-based. Setiap opcode membawa cost di IDL, dan quota dihitung dari total cost, bukan dari jumlah request. Lihat ADR-0006
Rate limit dijawab dengan RATE_LIMITED beserta retry_after_ms, bukan dengan menutup koneksi
Node yang kelebihan beban menjawab OVERLOADED dan mengirim RECONNECT_HINT ke node lain, bukan menerima beban sampai jatuh


161. ERROR CODE REGISTRY

STATUS: BUILT. Registry lengkap ada di shared/protocol/schema/errors.json dan digenerate ke Rust serta TypeScript.

Setiap error membawa code numerik stabil, symbol yang dapat dibaca mesin, message opsional, retry_after_ms bila relevan, dan field bila error berkaitan dengan satu field.

Class dan perilaku client:

1000 sampai 1099 Protocol
Tidak dapat diretry. Fatal. Client harus upgrade atau menutup koneksi.

1100 sampai 1199 Auth
Tidak dapat diretry langsung. Refresh token lalu login ulang.

1200 sampai 1299 Permission
Tidak dapat diretry. Tampilkan bahwa tindakan tidak diizinkan.

1300 sampai 1399 Validation
Tidak dapat diretry. Ini bug client. Catat dan laporkan.

1400 sampai 1499 RateLimit
Dapat diretry setelah retry_after_ms dengan jitter.

1500 sampai 1599 State
Tidak dapat diretry. Rekonsiliasi state lokal.

1600 sampai 1699 Server
Dapat diretry dengan backoff.

1700 sampai 1799 Federation
Dapat diretry. Tampilkan status degraded.

Kode yang penting untuk protocol:

1000 PROTOCOL_VERSION_UNSUPPORTED
1001 MALFORMED_FRAME
1002 UNKNOWN_OPCODE
1003 UNEXPECTED_OPCODE
1004 FRAME_TOO_LARGE
1005 UNSUPPORTED_FLAG
1006 DECODE_FAILED
1007 FEATURE_NOT_NEGOTIATED
1008 RESUME_REQUIRED
1009 SESSION_LAGGING
1503 SEQUENCE_GAP
1504 HISTORY_TRUNCATED
1507 PREKEYS_EXHAUSTED
1509 IDEMPOTENCY_MISMATCH
1606 FEATURE_DISABLED
1700 PEER_UNREACHABLE
1701 REGION_DEGRADED
1702 ROOM_READ_ONLY_PARTITION
1703 MESH_AUTH_FAILED
1704 ROUTING_EPOCH_STALE

Aturan:

Error TIDAK BOLEH dibuat dengan string bebas. Semua error berasal dari registry
Pesan internal error tidak pernah dikirim ke client. Hanya bagian public yang dikirim. Alamat IP, nama host, query database, dan stack trace TIDAK BOLEH sampai ke client
Kegagalan login memakai satu error yang sama untuk user tidak ada dan password salah. Perbedaan di antara keduanya adalah oracle keberadaan account, dan rate limiting tidak menutupnya
Code yang tidak dikenal client diperlakukan menurut class-nya berdasarkan range. Ini yang membuat client lama tetap berperilaku benar terhadap error baru


162. SECURITY MODEL OF THE PROTOCOL

STATUS: BUILT untuk primitive di migo-crypto dan untuk penolakan frame cacat di migo-wire. STATUS: SPEC untuk model ancaman penuh beserta pengujiannya.

Yang dilindungi transport, yaitu TLS 1.3 atau QUIC:

Metadata di kabel
Isi pesan Public Room dan Managed Room
Integritas frame terhadap pihak di jaringan

Yang dilindungi E2E:

Isi private message dan group message
Isi voice note private
SDP dan ICE candidate pada call private
Media voice dan video call

Yang TIDAK dilindungi dan harus diakui dengan jujur:

Fakta bahwa A berkomunikasi dengan B, waktunya, dan perkiraan ukuran pesan. Server memerlukan ini untuk routing
Isi Public Room dan Managed Room, karena server perlu membacanya untuk moderation
Metadata yang tercantum di section 10

Kewajiban validasi sebelum dispatch, dengan urutan tetap:

Frame version
Flag bit
Panjang frame terhadap MAX_FRAME_BYTES
Keberadaan opcode
Session state untuk opcode tersebut
Auth level untuk opcode tersebut
Rate limit cost
Baru kemudian decode payload

Urutan ini disengaja. Decode payload adalah bagian termahal, dan pekerjaan mahal tidak boleh dilakukan untuk frame yang akan ditolak.

Aturan keamanan protocol lainnya:

Frame dengan auth level Server yang datang dari socket client WAJIB ditolak dan dicatat sebagai insiden
Tidak ada transport plaintext, termasuk di development
Token bersifat opaque HMAC-SHA256, bukan JWT, sehingga tidak ada permukaan algoritma yang dapat dibingungkan
Refresh token bersifat rotating. Pemakaian ulang refresh token yang sudah ditukar dianggap pencurian dan seluruh family session dicabut, dijawab REFRESH_REUSE_DETECTED
Push token dan bot token disimpan dalam bentuk hash dan TIDAK BOLEH ditulis ke log
Data IP dipotong ke kelas jaringan dan disimpan maksimum 7 hari
Signed URL TIDAK BOLEH ditulis ke log atau analytics


163. E2E KEY MANAGEMENT PROTOCOL

STATUS: SCHEMA untuk KEY_PUBLISH dan KEY_BUNDLE_FETCH. STATUS: SPEC untuk group epoch dan call key.

Publikasi key. Client mengirim KEY_PUBLISH, opcode 16, berisi:

identity_key, yaitu Ed25519 signing key digabung X25519 exchange key, total 64 byte
signed_prekey_id
signed_prekey
signed_prekey_signature
signed_prekey_expires_at
Daftar one_time_prekey berisi pasangan key_id dan public key

Server memverifikasi bahwa signature signed prekey sah terhadap identity key, dan menolak dengan INVALID_KEY_MATERIAL bila tidak. Server juga menolak signed prekey yang sudah kedaluwarsa saat dipublikasikan.

Pengambilan key. Pengirim mengirim KEY_BUNDLE_FETCH, opcode 17, dan menerima KeyBundle berisi identity key, signed prekey beserta signature dan waktu kedaluwarsa, serta satu one-time prekey bila masih tersedia.

Aturan:

Satu one-time prekey dikonsumsi pada setiap pengambilan, dan tidak pernah diberikan dua kali
Ketika one-time prekey habis, session tetap dapat dibentuk dengan signed prekey saja. Server memberi penanda, dan client penerima diberitahu supaya mengisi ulang. Bila kebijakan menuntut, permintaan dijawab PREKEYS_EXHAUSTED
Client WAJIB mengisi ulang one-time prekey sebelum jumlahnya menipis, dan WAJIB merotasi signed prekey secara berkala
Key material milik device yang dicabut ditandai revoked dan tidak lagi diberikan

Rotasi dan cakupan:

Chat 1-on-1 memakai X3DH untuk pembentukan session lalu Double Ratchet untuk setiap pesan
Group memakai sender-key ratchet. Setiap perubahan keanggotaan menaikkan group_key_epoch dan mendistribusikan sender key baru, sehingga member yang keluar tidak dapat membaca pesan berikutnya
Call memakai key media yang diturunkan dari session E2E antar device, dirotasi saat peserta bergabung atau keluar melalui CALL_KEY_UPDATE

Yang TIDAK BOLEH:

Server menyimpan private key dalam bentuk apa pun
Key escrow, master key, atau recovery key milik server
Menurunkan key dari password saja tanpa material acak dari device
Memakai satu key untuk dua tujuan berbeda. Setiap tujuan memakai label HKDF berbeda


164. CLIENT KEY STORAGE REQUIREMENT

STATUS: SPEC. Belum ada client. Bagian ini adalah syarat yang WAJIB dipenuhi client pertama sebelum dianggap selesai.

Android:

Private key disimpan di Android Keystore
Key ditandai non-exportable
Operasi kriptografi dilakukan melalui Keystore untuk key type yang mendukungnya. Untuk primitive yang tidak didukung Keystore, key material dienkripsi dengan key yang berada di Keystore, dan plaintext key hanya ada di memori selama operasi
Backup otomatis sistem operasi WAJIB dikecualikan untuk file key
Screenshot dan screen recording SEBAIKNYA dibatasi pada layar yang menampilkan safety number

Web:

Private key disimpan sebagai CryptoKey non-extractable dari Web Crypto API, di dalam IndexedDB
Private key TIDAK BOLEH disimpan di localStorage, sessionStorage, cookie, URL, atau di dalam memori global yang dapat diakses script lain, baik plaintext maupun hasil encoding
Semua operasi kriptografi memakai Web Crypto. TIDAK BOLEH ada implementasi primitive kriptografi sendiri di JavaScript untuk jalur produksi
Content Security Policy ketat WAJIB aktif, tanpa inline script dan tanpa eval, karena satu XSS pada halaman yang memegang session sudah cukup untuk menyalahgunakan key walaupun key itu non-extractable
Service worker TIDAK BOLEH menyimpan plaintext pesan di cache yang dapat dibaca origin lain

iOS:

Keychain dengan atribut yang tidak ikut backup, dan Secure Enclave bila tersedia

Semua platform:

Kehilangan device berarti kehilangan key. Ini konsekuensi yang WAJIB dijelaskan kepada user sebelum ia bergantung padanya, bukan setelah
Verifikasi safety number tersedia di UI untuk setiap conversation private
Perubahan identity key peer memunculkan peringatan, tidak diterima diam-diam


165. CALL SIGNALING PROTOCOL

STATUS: SPEC. Requirement produk ada di section 180. Bagian ini adalah spesifikasi protokolnya.

Seluruh signaling memakai binary MWP/1 dengan opcode range calls, 224 sampai 239. JSON TIDAK BOLEH dipakai untuk signaling.

Alur panggilan 1-on-1:

Caller                    Gateway                     Callee
  |  CALL_INVITE 224  ------>|
  |                          |  CALL_INVITE_EVENT 225 --->|
  |<-- CallInviteResult ------|
  |                          |<---- CALL_ANSWER 226 ------|
  |<-- CALL_STATE_EVENT 232 --|
  |  CALL_SDP 230 offer ---->|--- CALL_SDP 230 offer ---->|
  |<-- CALL_SDP 230 answer --|<--- CALL_SDP 230 answer ---|
  |  CALL_ICE 231 --------->|--- CALL_ICE 231 --------->|
  |<-- CALL_ICE 231 --------|<--- CALL_ICE 231 ----------|
  |                                                       |
  |========== WebRTC P2P, E2E encrypted media ===========|
  |                                                       |
  |  CALL_END 229 ---------->|--- CALL_STATE_EVENT 232 -->|

Struct signaling. Semua field biner, tidak ada JSON:

CallInvite
call_id, conversation_id, callee_id, media_kind yaitu Audio atau Video, caller_device, capabilities bitmask, sealed_offer berupa bytes

CallInviteResult
call_id, status, expires_at

CallInviteEvent
call_id, conversation_id, caller_id, caller_device, media_kind, expires_at, sealed_offer

CallAnswer
call_id, callee_device, sealed_answer

CallDecline
call_id, reason

CallSdp
call_id, from_device, to_device, sealed_sdp

CallIce
call_id, from_device, to_device, sealed_candidates, dikirim sebagai batch, bukan satu frame per candidate

CallStateEvent
call_id, state yaitu Ringing, Connecting, Connected, Reconnecting, atau Ended, ditambah reason bila Ended

CallKeyUpdate
call_id, epoch, sealed_key_material

TurnCredentials
Daftar server berisi url, username, credential, ttl, dan region

Aturan wajib:

SDP dan ICE candidate WAJIB dienkripsi end-to-end antar device sebelum masuk ke server. Server meneruskan blob tersegel dan tidak mengurainya. SDP memuat sidik jari DTLS, alamat kandidat, dan kemampuan device, sehingga server yang membacanya akan mengetahui alamat jaringan kedua pihak
TIDAK BOLEH ada anonymous call signaling. Sebelum signaling diproses, server memverifikasi authentication, device, keanggotaan conversation, izin panggilan, status block, privacy setting, dan rate limit
ICE candidate dikirim dalam batch dengan linger singkat, bukan satu frame per candidate. Satu sesi ICE dapat menghasilkan puluhan candidate
call_id dibuat client dan menjadi idempotency key, sehingga retry invite tidak membuat dua panggilan
Invite memiliki expires_at. Invite yang kedaluwarsa berakhir dengan Ended tanpa perlu tindakan pengguna
Bila callee offline, server mengirim push notification berisi call_id dan penanda panggilan, bukan isi apa pun. Ringing lokal dimulai setelah client terhubung
CALL_STATS berkelas Droppable dan hanya memuat angka kualitas agregat. Isi panggilan, transkrip, dan sampel audio TIDAK BOLEH dikirim
Panggilan lintas region diteruskan melalui FED_CALL_RELAY, tetap sebagai blob tersegel


166. WEBRTC MEDIA ARCHITECTURE

STATUS: SPEC. Requirement produk ada di section 180.

Arsitektur media 1-on-1:

User A
   |
   +-- signaling --> Migo Gateway
   |
   +==================================> User B
            WebRTC P2P, E2E encrypted

Fallback ketika P2P gagal:

User A ==== encrypted ====> TURN ==== encrypted ====> User B

Group call:

User A --+
User B --+
User C --+--> Regional SFU --> peserta lain
User D --+
User E --+

Aturan:

P2P-first untuk seluruh panggilan 1-on-1. Media tidak melewati server Migo bila P2P berhasil
STUN dipakai untuk menemukan alamat publik dan melakukan NAT traversal. ICE memilih jalur terbaik
TURN dipakai hanya sebagai fallback ketika P2P gagal, misalnya karena symmetric NAT, carrier NAT, firewall perusahaan, atau UDP diblokir
TURN adalah relay dan TIDAK BOLEH dapat membaca plaintext media. Media tetap terenkripsi ujung ke ujung saat melewatinya
TURN dideploy per region. Client memilih berdasarkan latensi dan ketersediaan, dengan urutan primary, secondary, lalu region lain
Kredensial TURN bersifat sementara, diberikan lewat CALL_TURN_FETCH, dan TIDAK BOLEH ditanam di dalam aplikasi
Group call memakai SFU yang hanya meneruskan paket. SFU TIDAK BOLEH memiliki akses ke plaintext media. E2E untuk group call memakai enkripsi frame media dengan key yang dibagikan antar peserta, dirotasi melalui CALL_KEY_UPDATE saat keanggotaan berubah
MCU yang melakukan transcoding TIDAK BOLEH dipakai untuk panggilan yang diklaim E2E, karena transcoding menuntut akses plaintext

Codec dan adaptasi:

Audio memakai codec speech modern dengan bitrate rendah
Video memakai adaptive bitrate berdasarkan resolusi, FPS, dan kondisi jaringan
TIDAK BOLEH ada satu bitrate yang dipaksakan untuk semua device
Pada low-data mode, audio diprioritaskan, video diturunkan resolusi dan FPS, HD dimatikan
Perubahan jaringan memicu ICE restart melalui CALL_RENEGOTIATE, bukan panggilan baru

Target bandwidth call ada di section 171.


167. VOICE NOTE PROTOCOL

STATUS: SPEC. Requirement produk ada di section 179.

Voice note bukan jenis pesan terpisah. Voice note adalah MESSAGE_SEND dengan kind bernilai Voice, sehingga seluruh mekanisme urutan, dedup, offline queue, receipt, dan sync dipakai ulang tanpa jalur paralel yang harus dijaga sendiri.

Alur pengiriman:

Record
|
Encode dengan codec speech
|
Hitung waveform ringkas
|
Encrypt di client dengan key acak per media
|
MEDIA_UPLOAD_BEGIN 128, mendapat upload ticket
|
Chunked upload ke object storage, dapat di-resume
|
MEDIA_UPLOAD_COMMIT 130, mendapat media_id
|
MESSAGE_SEND 32 dengan kind Voice, envelope memuat media_id, key media, durasi, dan waveform
|
Penerima menerima MESSAGE_EVENT 33
|
MEDIA_FETCH_URL 132, mendapat signed URL berumur pendek
|
Download ciphertext lalu decrypt lokal
|
Playback

Isi envelope voice note, berada di dalam ciphertext sehingga tidak dapat dibaca server:

media_id
media_key
media_nonce
content_hash dari ciphertext
duration_ms
sample_rate
codec
waveform, sebagai array u8 dengan jumlah bucket tetap, misalnya 64 bucket, sehingga biayanya puluhan byte dan bukan kilobyte

Yang diketahui server:

message_id, conversation_id, kind Voice, sender, seq, created_at, media_id, ukuran object, dan waktu upload

Aturan wajib:

Voice note pada private chat dan group chat WAJIB dienkripsi di client sebelum upload. Server tidak pernah menerima audio plaintext
Waveform dihitung di client sebelum enkripsi. Server tidak dapat menghitungnya, dan itu memang tujuannya
Codec WAJIB codec speech dengan bitrate rendah. Format lossless besar TIDAK BOLEH menjadi default
Upload WAJIB chunked dan dapat di-resume. Kegagalan pada 80 persen dilanjutkan dari sekitar 80 persen, bukan dari nol. MEDIA_UPLOAD_STATUS 129 dipakai untuk menanyakan posisi terakhir
Offline queue WAJIB durable di device. Rekaman yang dibuat saat offline tidak boleh hilang karena aplikasi ditutup
Playback speed 1x, 1.5x, dan 2x dilakukan di client tanpa meminta ulang media
Cache lokal menyimpan hasil dekripsi dengan batas ukuran dan kebijakan penghapusan otomatis. Cache WAJIB berada di penyimpanan privat aplikasi
Low-data mode mematikan auto-download voice note. Voice note diunduh saat user menekan play
Signed URL berumur pendek dan diminta saat akan diputar, bukan disimpan
Transkripsi dan translation untuk voice note E2E hanya boleh dilakukan di device. Mengirim audio ke server untuk transkripsi akan membatalkan klaim E2E, jadi bila fitur ini memakai layanan server, fitur tersebut WAJIB dinyatakan tidak E2E dan meminta izin eksplisit user
Voice note di Public Room dan Managed Room dapat dibaca server dan WAJIB melewati moderation. Perbedaan ini WAJIB terlihat di UI


168. MEDIA ARCHITECTURE AND SIGNED URL

STATUS: SPEC untuk opcode media. Kebijakan sudah final.

Alur upload:

Client meminta MEDIA_UPLOAD_BEGIN dengan ukuran, tipe, dan tujuan
Server memeriksa quota, batas ukuran, dan izin, lalu menerbitkan upload ticket berisi signed URL, upload_id, ukuran chunk, dan masa berlaku
Client mengunggah chunk langsung ke object storage
Client memanggil MEDIA_UPLOAD_COMMIT, dan server memverifikasi ukuran serta content hash lalu membuat record media
Media yang tidak di-commit dalam batas waktu tertentu dibersihkan oleh job terjadwal

Alur download:

Client meminta MEDIA_FETCH_URL dengan media_id
Server memeriksa otorisasi, yaitu apakah pemohon adalah anggota conversation atau room yang memuat media tersebut
Server menerbitkan signed URL berumur pendek
Client mengunduh langsung dari object storage

Aturan:

Chat server TIDAK BOLEH menjadi proxy byte media
Bucket TIDAK BOLEH public dan TIDAK BOLEH memiliki URL permanen
Signed URL berumur pendek, sekali pakai bila storage mendukungnya, dan TIDAK BOLEH masuk log, analytics, atau crash report
Otorisasi diperiksa saat URL diterbitkan, bukan hanya saat record dibuat
Scan status media yang dapat dibaca server memiliki tiga nilai: pending, clean, rejected. Media pending TIDAK BOLEH disajikan ke pengguna lain
Media E2E tidak dapat discan server. Perlindungannya ada di client, yaitu batas ukuran, validasi tipe setelah dekripsi, sandbox saat render, dan pelaporan oleh user
Thumbnail untuk media E2E dibuat di client dan dienkripsi seperti media utamanya


169. FEDERATION PACKET PROTOCOL

STATUS: SPEC. Keputusan desain ada di ADR-0005 dan section 7.

Transport:

TLS 1.3 di atas TCP sebagai transport default, QUIC/TLS 1.3 sebagai opsi kedua bila tersedia
Framing pada stream memakai length prefix u32 big-endian
Semua packet adalah binary MWP/1 dengan opcode range federation, 208 sampai 223
Listener mesh berada di network segment terpisah dan TIDAK BOLEH terekspos ke public Internet

Handshake:

Node A                                Node B
  |  FED_HELLO 208 { version, node_id, nonce_a, capabilities, epoch } -->
  |<-- FED_HELLO 208 { version, node_id, nonce_b, capabilities, epoch } --
  |  FED_AUTH 209 { signature_a } -->
  |<-- FED_AUTH 209 { signature_b } --
  |<-- FedWelcome sebagai response FED_AUTH --
  |  FED_PING 210 secara periodik -->

Signature WAJIB dibuat di atas gabungan berikut, dengan panjang setiap bagian diberi prefix supaya tidak ada ambiguitas batas:

Domain separation string "migo-mesh-v1"
Protocol version
Signer node id
Peer node id
nonce_a
nonce_b
timestamp

Validasi:

Node id WAJIB ada di allow-list berbasis public key
Timestamp WAJIB berada dalam toleransi 60 detik
nonce WAJIB acak 32 byte dan tidak boleh berulang selama jendela toleransi
Sequence number per link WAJIB naik. Packet dengan sequence tidak lebih besar dari sequence terakhir ditolak dan dicatat
Kegagalan autentikasi dijawab MESH_AUTH_FAILED lalu koneksi ditutup

Packet operasional:

FED_FORWARD 211
Meneruskan pesan atau event antar region. Payload private tetap berupa cryptographic envelope yang tidak dapat dibaca node perantara.

FED_ACK 212
Watermark kumulatif per link.

FED_ROOM_SUBSCRIBE 213 dan FED_ROOM_EVENT 214
Node yang memiliki anggota sebuah room berlangganan event room tersebut dari home region room.

FED_PRESENCE_DIGEST 215
Presence antar region dikirim sebagai digest berkala dan teragregasi, bukan per perubahan per user. Presence lintas region adalah sumber traffic mesh terbesar bila dikirim mentah.

FED_KEY_ROTATE 216
Rotasi key node dengan masa tumpang tindih. Key baru diumumkan lebih dulu, kedua key diterima selama jendela rotasi, baru key lama dicabut. Rotasi tanpa tumpang tindih akan memutus seluruh mesh sesaat.

FED_HEALTH 217
Health check berkala berisi status role, lag, dan kapasitas.

FED_SHARD_MAP 218
Distribusi peta shard room beserta routing epoch.

FED_ERROR 219
Error dengan code dari registry yang sama.

FED_CALL_RELAY 220
Meneruskan signaling call lintas region sebagai blob tersegel.

FED_DIRECTORY 221
Discovery node dan region.

Aturan:

Federation TIDAK BOLEH memakai JSON
Node perantara TIDAK BOLEH dapat membaca isi private message. Ia hanya melihat envelope
Setiap link memiliki rate limit dan connection limit sendiri
Packet dari node yang tidak dikenal ditolak sebelum decode payload


170. FEDERATION ROUTING, DISCOVERY, FAILOVER, SHARDING

STATUS: SPEC.

Regional discovery:

Setiap region memiliki daftar node yang diketahui melalui konfigurasi dan FED_DIRECTORY
Node baru bergabung dengan proses join yang eksplisit dan disetujui, bukan dengan auto-discovery terbuka. Auto-discovery terbuka di mesh yang membawa pesan pengguna adalah permukaan serangan yang tidak perlu
Client diarahkan ke gateway terdekat berdasarkan latensi terukur, bukan hanya berdasarkan GeoIP

Routing:

Setiap room memiliki home region. Sequencer room hanya ada satu, yaitu di home region
Pesan dari region lain diteruskan ke home region untuk mendapatkan seq, lalu disebarkan kembali. Ini menjaga urutan tunggal
Routing memakai routing epoch. Node yang memakai epoch lama dijawab ROUTING_EPOCH_STALE dan memperbarui peta

Room sharding:

Room besar dibagi ke beberapa node berdasarkan room_id
Peta shard didistribusikan melalui FED_SHARD_MAP
Perpindahan shard dilakukan dengan drain, yaitu node lama berhenti menerima anggota baru, anggota diarahkan pindah dengan RECONNECT_HINT, baru shard dialihkan
Room dengan puluhan ribu anggota memakai fan-out bertingkat, yaitu satu node menerima event lalu meneruskan ke node lain yang memegang sebagian anggota, bukan satu node mengirim ke semua

Health check dan failover:

FED_HEALTH berkala per link
Node yang gagal health check ditandai degraded dan dikeluarkan dari routing untuk traffic baru, tetapi koneksi yang berjalan diberi kesempatan drain
Kegagalan region dijawab REGION_DEGRADED, dan client diarahkan ke region terdekat berikutnya
Bila home region sebuah room tidak terjangkau, room menjadi read-only dan dijawab ROOM_READ_ONLY_PARTITION. TIDAK BOLEH ada sequencer kedua yang diangkat, karena dua sequencer berarti dua urutan yang tidak dapat digabungkan tanpa kehilangan pesan
Private message tetap dapat dikirim saat region tujuan tidak terjangkau, karena pesan disimpan di region pengirim dan diteruskan ketika link kembali. Inilah alasan private message tidak memerlukan sequencer global


171. BANDWIDTH TARGET AND BUDGET

STATUS: SPEC untuk gate CI. Angka pada bagian ini sudah dapat diukur dari encoder migo-wire, tetapi pengukurannya belum otomatis.

Target per event dan per session ada di section 56. Bagian ini menambahkan target untuk call, voice note, media, dan federation, serta cara pengukurannya.

Voice note:

Overhead protocol untuk mengirim voice note, di luar byte audio
Maksimum 256 byte, termasuk envelope terenkripsi yang memuat media_id, key, durasi, dan waveform 64 bucket

Audio voice note
Codec speech dengan bitrate rendah. Rekaman 10 detik SEBAIKNYA berada di bawah 20 KB

Waveform
Maksimum 64 byte

Call signaling:

Seluruh signaling satu panggilan yang berhasil, dari invite sampai connected
Maksimum 8 KB termasuk SDP dan ICE candidate yang sudah tersegel

CALL_ICE per batch
Maksimum 1 KB

CALL_STATS per laporan
Maksimum 128 byte, dan berkelas Droppable

Call media:

Audio 1-on-1
Sekitar 20 sampai 80 kbps tergantung codec dan jaringan

Video 1-on-1
Variable bitrate berdasarkan resolusi, FPS, dan jaringan. TIDAK BOLEH ada satu bitrate tetap untuk semua device

Low-data mode
Audio diprioritaskan. Video diturunkan resolusi dan FPS. HD dimatikan

Federation:

FED_PRESENCE_DIGEST
Digest teragregasi per interval, bukan per perubahan. Maksimum satu digest per region per interval per room aktif

FED_FORWARD
Overhead di luar envelope maksimum 64 byte

FED_ACK
Watermark kumulatif, maksimum 16 byte

Cara pengukuran:

Gateway mengekspor migo_frames_total, migo_frame_bytes_bucket, dan migo_dropped_frames_total, semuanya berlabel opcode dan delivery class. Regresi terlihat sebagai pergeseran byte per pesan
tools/loadgen melaporkan byte per user per menit untuk setiap skenario, dan CI menggagalkan job performa bila sebuah skenario melewati budget lebih dari 10 persen
Web client mencatat penghitung byte per session pada mode development, sehingga biaya sebuah fitur terlihat saat fitur itu ditulis, bukan setelah diluncurkan
Setiap penambahan opcode WAJIB disertai satu test yang mengukur ukuran frame tipikalnya dan membandingkannya dengan budget


172. PROTOCOL TESTING: LOAD, STRESS, FUZZ, SECURITY

STATUS: BUILT untuk unit test di migo-wire, migo-core, migo-crypto, dan migo-store. STATUS: SPEC untuk property test, conformance vector, fuzz, load, stress, dan security test. Catatan: proptest sudah menjadi dev-dependency tetapi belum dipakai, sehingga belum boleh dihitung sebagai BUILT.

Conformance:

Test vector biner di shared/protocol/vectors WAJIB dihasilkan identik oleh Rust dan TypeScript. Perbedaan satu byte adalah kegagalan build
Round-trip encode lalu decode untuk setiap struct
Property test: struct apa pun yang dienkode lalu didecode menghasilkan nilai yang sama
Test kompatibilitas maju: payload dengan optional field yang tidak dikenal dilewati dengan benar oleh decoder lama
Test kompatibilitas mundur: payload tanpa optional field baru tetap valid untuk decoder baru

Fuzz:

Fuzz decoder frame dengan byte acak dan dengan mutasi dari corpus vector yang valid
Fuzz setiap struct decoder
Target: tidak ada panic, tidak ada alokasi tak terbatas, tidak ada loop tak berujung, tidak ada stack overflow, untuk input apa pun
Kasus yang WAJIB ada di corpus: varint tidak kanonik, varint 11 byte, length yang lebih besar dari frame, nesting 17 tingkat, list count dua miliar, string UTF-8 tidak valid, trailing byte, flag reserved bernilai satu, opcode nol, correlation sangat besar, BATCH berisi BATCH, FRAGMENT dengan total nol
Fuzz dijalankan di CI dengan durasi terbatas, dan dijalankan lebih panjang pada jadwal harian

Load:

Skenario yang WAJIB diukur: sepuluh ribu session idle, seribu pesan per detik pada satu region, satu room dengan 10 ribu anggota, seribu panggilan bersamaan, seribu upload voice note bersamaan, dan sinkronisasi massal setelah pemadaman
Metrik yang diperiksa: latensi p50, p95, dan p99, byte per user per menit, penggunaan memori per session, dan jumlah frame yang dibuang

Stress dan kondisi buruk:

Client lambat yang tidak pernah membaca socket. Harapan: session ditutup dengan SessionLagging, memori node tidak tumbuh
Client yang mengirim sangat cepat. Harapan: RATE_LIMITED dengan retry_after_ms, koneksi tidak ditutup
Jaringan dengan packet loss 20 persen dan RTT 500 ms. Harapan: resume bekerja, tidak ada pesan ganda, tidak ada pesan hilang
Perpindahan jaringan berulang. Harapan: reconnect segera, tidak ada badai reconnect
Reconnect massal sepuluh ribu client bersamaan. Harapan: full jitter menyebar beban, node tidak jatuh

Security:

Test bahwa frame dengan auth level Server ditolak dari socket client
Test bahwa opcode dengan auth level User ditolak sebelum autentikasi
Test bahwa server tidak pernah menerima plaintext untuk conversation private
Test bahwa associated data mengikat metadata, sehingga ciphertext tidak dapat dipindah ke conversation lain
Test bahwa error internal tidak membocorkan alamat, query, atau stack trace
Test bahwa signed URL tidak muncul di log
Test bahwa refresh token yang dipakai ulang mencabut seluruh family session
Test bahwa handshake mesh menolak signature dengan node id yang ditukar, timestamp di luar toleransi, dan nonce yang diulang
Test bahwa migod menolak start pada production dengan secret kosong atau default


173. MULTI-REGION FAILURE TESTING

STATUS: SPEC. Memerlukan migod yang belum ditulis.

Skenario yang WAJIB diuji, masing-masing dengan harapan yang eksplisit:

Satu node gateway mati mendadak
Client reconnect dengan resume, tidak ada pesan hilang, tidak ada pesan ganda

Satu region terputus dari region lain, yaitu split brain
Room yang home region-nya tidak terjangkau menjadi read-only dan menjawab ROOM_READ_ONLY_PARTITION. Tidak ada sequencer kedua yang muncul. Private message tetap terkirim setelah link kembali

Link antar region lambat, bukan mati
Presence digest tetap dalam budget, FED_ACK tidak menumpuk tanpa batas, dan node degraded ditandai sebelum kehabisan memori

Database primary gagal dan failover
Penulisan gagal dengan STORAGE_UNAVAILABLE, client retry dengan backoff, tidak ada pesan yang tercatat dua kali setelah pemulihan

Redis hilang seluruhnya
Presence dan typing hilang lalu terbentuk kembali. Tidak ada pesan yang hilang. Ini yang membuat Redis boleh dianggap ephemeral

Object storage tidak tersedia
Chat tetap berjalan. Media gagal dengan MEDIA_UNAVAILABLE dan masuk queue, bukan menggagalkan pengiriman pesan

Clock skew antar node
Handshake mesh menolak di luar toleransi 60 detik. Urutan pesan tetap benar karena urutan memakai seq, bukan waktu

Rolling deploy dengan dua versi protocol berjalan bersamaan
Client versi lama dan baru dapat berkomunikasi. Tidak ada frame yang ditolak karena optional field baru

Rebalance shard room saat room aktif
Anggota berpindah node dengan RECONNECT_HINT tanpa kehilangan pesan

Pemadaman region penuh lalu pemulihan
Sinkronisasi massal setelah pemulihan tidak melampaui kapasitas. Client memakai backoff dengan jitter dan sync inkremental, bukan full resync

Semua skenario di atas WAJIB dapat dijalankan sebagai test otomatis. Simulasi deterministik dengan Clock, Random, dan Transport yang disuntikkan dipakai supaya kegagalan dapat diulang dengan seed yang sama, sesuai ADR-0009. Kegagalan yang tidak dapat direproduksi tidak dapat diperbaiki dengan percaya diri.


174. PROTOCOL OBSERVABILITY, METRICS, LOGGING

STATUS: SPEC. Nama metric sudah ditetapkan di sini dan di docs/05-bandwidth-budget.md, tetapi belum ada exporter.

Metrics wajib:

migo_frames_total, berlabel opcode, direction, dan delivery class
migo_frame_bytes_bucket, berlabel opcode
migo_dropped_frames_total, berlabel opcode dan alasan
migo_sessions_active
migo_session_lagging_total
migo_resume_total, berlabel hasil yaitu resumed atau required
migo_reconnect_total, berlabel reason
migo_decode_errors_total, berlabel error symbol
migo_rate_limited_total, berlabel opcode
migo_errors_total, berlabel error symbol dan class
migo_e2e_prekeys_remaining, sebagai histogram per account
migo_call_setup_seconds
migo_call_p2p_success_ratio
migo_call_turn_fallback_total
migo_media_upload_bytes_total dan migo_media_upload_resume_total
migo_federation_link_up, migo_federation_lag_seconds, migo_federation_replay_rejected_total
migo_conversation_seq_gap_total

Tracing:

Flag TRACED membawa 16 byte trace id dan 8 byte span id
Tracing di-sampling, tidak selalu aktif, karena 24 byte per frame melanggar budget bila selalu dikirim
Trace context diteruskan melalui federation, sehingga satu pesan lintas region dapat dilihat sebagai satu jejak

Logging. Yang TIDAK BOLEH masuk log, tanpa pengecualian:

Plaintext pesan
Isi envelope
Private key dan key material apa pun
Access token, refresh token, bot token, push token dalam bentuk mentah
Signed URL
Password, bahkan yang salah
Alamat IP penuh. IP dipotong ke kelas jaringan dan disimpan maksimum 7 hari
SDP dan ICE candidate, karena keduanya memuat alamat jaringan pengguna

Yang boleh masuk log:

opcode, ukuran frame, correlation, session id, node id
error code dan symbol
durasi pemrosesan
account id dan device id, karena keduanya diperlukan untuk dukungan pengguna
hash dari token, bukan token itu sendiri

Aturan:

Log berbentuk terstruktur, dan JSON diperbolehkan di sini karena log bukan wire protocol
Level log default pada production adalah info. Debug TIDAK BOLEH aktif secara permanen di production, karena debug cenderung mencatat payload
Setiap error yang dikembalikan ke client memiliki pasangan entri log dengan detail internal, sehingga dukungan pengguna dapat menghubungkan keluhan dengan penyebab tanpa membocorkan detail ke client


175. PROTOCOL DEPLOYMENT AND ROLLOUT

STATUS: SPEC. Memerlukan infra yang belum ditulis.

Prinsip: protocol berubah lebih lambat daripada aplikasi, dan client selalu tertinggal di belakang server.

Urutan rollout untuk perubahan additive:

Ubah IDL di shared/protocol/schema
Jalankan generator, commit hasil generate
Implementasikan sisi server dan sisi client, di belakang feature bit
Deploy server yang sudah mengerti fitur baru tetapi belum mengiklankannya
Aktifkan pengiklanan feature bit secara bertahap per region atau per persentase session
Rilis client yang mengiklankan feature bit yang sama
Pantau metrics selama satu siklus penuh sebelum menganggap fitur stabil

Aturan:

Server WAJIB dideploy lebih dulu daripada client. Client yang meminta fitur yang belum ada di server hanya berarti negosiasi menolak fitur tersebut, sedangkan server yang mengirim fitur yang tidak dimengerti client berarti kerusakan
Rolling deploy WAJIB dapat menjalankan dua versi build secara bersamaan tanpa masalah
Node yang di-drain mengirim RECONNECT_HINT dengan after_ms yang diacak, lalu menutup setelah outbound queue kosong atau deadline tercapai
Kill switch per fitur WAJIB ada, dan mematikannya berarti berhenti mengiklankan feature bit, bukan mengirim error di tengah sesi yang sudah berjalan
Rollback server WAJIB aman, artinya server versi lama tidak boleh gagal membaca data yang ditulis server versi baru


176. PROTOCOL MIGRATION STRATEGY

STATUS: SPEC. Belum ada versi protokol kedua, sehingga jalur migrasi belum pernah dijalankan.

Migrasi di dalam MWP/1, yaitu perubahan additive:

Tidak memerlukan langkah khusus. Optional field baru dilewati decoder lama, enum baru menjadi Unknown, opcode baru dijawab UNKNOWN_OPCODE oleh server lama sehingga client dapat mundur ke jalur lama

Migrasi ke MWP/2, yaitu perubahan yang merusak:

Server WAJIB melayani v1 dan v2 secara bersamaan minimal satu siklus deprecation client penuh
Byte version pada frame yang menentukan parser mana yang dipakai. Tidak ada penebakan
Session tidak dapat berpindah versi di tengah jalan. Perpindahan versi terjadi pada koneksi baru
Client lama menerima peringatan upgrade melalui NOTIFICATION_EVENT dan melalui Welcome, bukan diputus tanpa penjelasan
Tanggal akhir dukungan v1 diumumkan lebih dulu, dan setelah tanggal itu v1 dijawab PROTOCOL_VERSION_UNSUPPORTED dengan pesan yang menjelaskan cara upgrade
Metrik jumlah session per versi protocol dipakai untuk memutuskan kapan v1 boleh dimatikan. Keputusan diambil dari angka, bukan dari perkiraan

Migrasi data yang berkaitan dengan protocol:

Perubahan format cryptographic envelope memerlukan envelope_version baru, dan client WAJIB tetap dapat membaca versi lama karena riwayat pesan lama tidak dapat dienkode ulang oleh server
Perubahan format penyimpanan key di device WAJIB dapat membaca format lama
Perubahan skema database mengikuti section 126


177. IMPLEMENTATION STATUS

STATUS: BUILT. Bagian ini adalah daftar status itu sendiri dan WAJIB akurat pada setiap commit.

Bagian ini ada supaya dokumen tidak pernah mengklaim sesuatu yang belum dibangun. Status per 2026-08-27.

BUILT, sudah ada kode dan test yang lulus:

migo-core, yaitu error type, config, id, timestamp, secret, dan clock
migo-wire, yaitu frame encode dan decode, flags, varint, MSE codec, dan limits
migo-protocol, yaitu hasil generate untuk struct, enum, opcode, error code, dan helper fault
migo-crypto, yaitu primitive terenkapsulasi, HKDF label, AEAD, Argon2id, token HMAC, dan mesh signature
migo-store, yaitu 10 storage trait domain, backend in-memory, backend PostgreSQL di atas ORM SeaORM dengan 29 entity hasil generate dari file migration sehingga tidak ada daftar kolom yang ditulis tangan, dan satu contract test suite yang dijalankan terhadap keduanya sehingga tidak ada case yang hanya terpasang di satu backend
migo-cache, yaitu 6 cache trait domain untuk key value, counter, token bucket, presence, typing, dan session routing, backend in-memory, backend Redis dengan Lua script untuk operasi atomik, satu contract test suite berisi 48 case yang dijalankan terhadap keduanya, dan penstempelan keyspace per run supaya key sisa dari run sebelumnya tidak pernah terbaca sebagai milik run ini
migo-ratelimit, yaitu satu engine token bucket berbasis cost di atas 7 surface pada section 120, cost per opcode dibaca dari IDL, trust tier menskalakan capacity dan refill, fallback bucket lokal yang lebih ketat saat cache tidak tersedia, validasi konfigurasi saat startup supaya bucket yang tidak akan pernah cukup untuk operasinya ditolak sebelum melayani, dan 34 test
migo-auth, yaitu registrasi, sign in lewat username atau email, format access token 130 byte yang diverifikasi dengan satu MAC tanpa membaca database sama sekali, rotasi refresh token dengan deteksi reuse yang mematikan seluruh family, batas jumlah device per akun, pencabutan session dan device yang berlaku pada request berikutnya bukan pada saat token kedaluwarsa, penyimpanan authenticated_at per session supaya operasi sensitif dapat menuntut password diketik ulang, harga percobaan gagal yang diturunkan dari bucket anonim sehingga selalu benar-benar terpungut, verifikasi terhadap hash placeholder pada akun yang tidak ada supaya waktu respons tidak membocorkan keberadaan akun, dan 67 test
migo-messaging, yaitu percakapan direct dan group, penomoran sequence yang gapless dan tidak pernah dipakai ulang dengan tombstone yang mempertahankan sequence-nya sesuai section 67, deduplikasi berdasarkan message_id sehingga retry menghasilkan sukses dengan duplicate true dan bukan error sesuai section 68, penolakan bila satu message_id dipakai untuk pesan yang berbeda, receipt delivered dan read yang hanya bergerak maju dengan read yang menyiratkan delivered sesuai section 158, sync maju dan mundur dengan status Truncated yang dikatakan apa adanya ketika history sudah hilang alih-alih menyerahkan history yang lebih pendek yang tampak lengkap, daftar percakapan dengan cursor opaque berbasis keyset sesuai section 157 sehingga percakapan yang menerima pesan di tengah paging tidak membuat satu baris muncul dua kali atau hilang, limit yang selalu di-clamp dan tidak pernah ditolak, typing indicator yang hanya hidup di cache dengan TTL 10 detik sesuai section 15, penekanan frame ketika tidak ada state yang berubah sesuai section 156, penyapuan disappearing message, dan 39 test. Crate ini mengembalikan rencana fanout bukan mengirimkan apa pun, sehingga gateway dapat melakukan encode satu kali lalu mengirim ke N socket, dan seluruh aturan urutannya dapat diuji tanpa jaringan. Empat penyimpangan yang sengaja diambil dan wajib dibaca bersama section 145: sender_key_id diterima pada MESSAGE_SEND tetapi tidak pernah dikembalikan pada MESSAGE_EVENT karena salinan yang berwenang sudah berada di dalam envelope dan terikat AEAD sesuai section 11, sehingga salinan kedua di field plaintext akan tidak terautentikasi dan memberi client pilihan mana yang dipercaya; MESSAGE_DELETE dengan for_everyone false ditolak dengan FEATURE_DISABLED karena tidak ada tabel hide per member dan sukses yang tidak melakukan apa pun adalah satu-satunya jawaban yang pasti salah; title dan avatar_url pada ConversationSummary selalu kosong karena nama milik agregat room dan signed URL milik migo-media yang tidak boleh melewati layer yang menulis log sesuai section 174, sedangkan mengambilnya per baris akan menjadi N+1 pada daftar 200 baris; dan opcode 40 sampai 42 yaitu edit belum termasuk karena sebuah edit bukan append dengan kata kerja lain melainkan memerlukan riwayat edit untuk moderation, aturan untuk reply yang mengutipnya, dan keputusan apakah edit memicu notifikasi ulang
migo-presence, yaitu presence per device yang seluruh state-nya hidup di cache dengan TTL sebesar tiga kali heartbeat yang diumumkan kepada session itu sendiri sehingga session pada UltraLowData yang disuruh heartbeat empat kali lebih lambat mendapat masa hidup empat kali lebih panjang dan tidak berkedip offline di antara dua heartbeat yang keduanya tepat waktu, proyeksi state terkuat lintas device dengan Busy di atas Online karena Busy hanya pernah diset dengan sengaja, penegakan Invisible di server dengan cara memproyeksikannya menjadi Offline sebelum sebuah frame ada sehingga tidak ada satu pun jalur kode yang menerbitkan Invisible, pewarisan Invisible oleh device yang menyambung ulang supaya user yang bersembunyi di ponselnya tidak tersingkap oleh laptopnya yang reconnect, jawaban tentang diri sendiri yang justru tidak diproyeksikan karena user boleh melihat bahwa dirinya sedang tersembunyi, penekanan frame ketika tidak ada yang berubah sesuai section 156 yang merupakan hasil normal dari setiap heartbeat dan dari setiap disconnect yang bukan disconnect terakhir, penanda since yang bertahan melewati refresh sehingga online sejak jam sembilan tidak ter-reset setiap heartbeat, kebangkitan entry yang kedaluwarsa di bawah socket yang masih hidup, snapshot kontak yang menghapus id kembar dalam urutan pemanggil dan di-clamp ke 512 subject dengan satu kali round trip ke cache, dan 26 test. Crate ini mengembalikan rencana fanout bukan mengirimkan apa pun, sama seperti migo-messaging, dan audiensnya adalah topic akun subject itu sendiri karena menghitung audiens di sini berarti membaca setiap conversation dan room pada setiap heartbeat. Satu-satunya tulisan durable yang dilakukan crate ini adalah penanda last seen pada baris device saat disconnect yang rapi, karena satu tulisan baris per device per heartbeat adalah write amplification yang besar untuk sebuah field yang dirender sebagai terakhir terlihat dua jam lalu. Field last_seen hanya diisi pada jalur baca, hanya untuk subject yang sama sekali tidak punya entry hidup, tunduk pada setelan show_last_seen milik subject dengan Friends yang menuntut pertemanan yang sudah diterima karena permintaan yang masih menggantung bukan relasi, dan dibatasi 64 pencarian per snapshot supaya subscribe ke sebuah room tidak berubah menjadi dua ratus round trip. Lima penyimpangan yang sengaja diambil: custom_status ditolak dengan FEATURE_DISABLED alih-alih diterima ke dalam presence entry karena sebuah custom status diharapkan hidup lebih lama daripada satu disconnect sedangkan segala sesuatu di crate ini menguap bersama cache, sehingga menyimpannya di sini akan membuat janji section 173 diam-diam menjadi tidak benar dan rumahnya adalah kolom profile; aggregate count presence untuk room besar tidak dihitung di sini melainkan berasal dari himpunan subscriber di gateway, karena menghitungnya di sini berarti membaca roster 800 baris pada setiap subscribe yang justru dilarang section 14; interval minimum diumumkan tetapi tidak ditegakkan di sini dengan alasan yang ditulis di section 159; tidak ada deteksi idle otomatis karena hanya client yang tahu apakah jendelanya sedang fokus dan server yang menaikkan Online menjadi Away sendiri juga harus menurunkannya lagi, yaitu satu timer per session untuk fakta yang sudah dipegang client; dan digest presence antar region lewat FED_PRESENCE_DIGEST pada section 169 adalah milik crate mesh, bukan milik crate ini
migo-economy, yaitu trait Treasurer dengan 15 metode untuk listings, listing, wallet, statement, purchase, send_gift, entitlements, gifts_received, gift_shelf, progression, badges, leaderboard, grant, award, dan award_badge, ditambah trait port Announcer dengan 1 metode yang diimplementasikan composition root sehingga crate ini tidak pernah menaut migo-notify, dengan bentuk yang sama seperti Storage pada migo-media, Roster pada migo-moderation, dan PushSender pada migo-notify, dan Silent adalah implementasinya bagi deployment maupun test tanpa layanan notifikasi. Setiap perpindahan nilai adalah transaksi yang leg-nya berjumlah nol sehingga currency tidak diciptakan atau dimusnahkan oleh sebuah gift maupun pembelian melainkan hanya berpindah antar akun, dan currency hanya masuk ke sistem dari akun mint yaitu satu-satunya akun yang boleh bernilai negatif, sehingga jumlah setiap saldo nyata selalu tepat sama dengan yang telah dikeluarkan mint dan itulah invariant yang diperiksa audit terjadwal lewat EconomyStore::currency_sum, sebab currency yang jumlahnya menyimpang dari nol adalah bug yang tertangkap sebelum siapa pun sempat membelanjakan selisihnya. Points yaitu reputasi yang diberikan sebuah gift dicetak dengan cara yang sama tetapi sengaja tidak dapat dipindahkan dan tidak dapat dibelanjakan, karena section 87 melarang apa pun yang menyerupai cash-out tanpa tinjauan regulator dan section 37 melarang perjudian uang nyata, sedangkan skor reputasi yang dapat berpindah antar akun atau ditukar dengan currency yang dapat dibelanjakan adalah langkah pertama ke arah keduanya, sehingga points naik dan tidak pernah menyamping. Pemanggil dibagi menjadi dua bentuk metode: yang dijangkau client selalu menerima Caller karena ia membelanjakan uangnya sendiri atau membaca standing-nya sendiri dan rate limiter memungut budget-nya, sedangkan tiga metode yang tidak yaitu grant, award, dan award_badge adalah server yang mengkredit seseorang atas peristiwa yang telah ia amati; client tidak dapat meminta XP, currency, maupun badge karena tidak ada metode untuk memintanya, dan crate yang mengamati game atau peristiwa memanggil ketiganya lewat port yang ia sendiri miliki sehingga tidak ada panah dari layer ini yang menyamping ke crate layer 3 lain, dengan bentuk inversi yang sama seperti Announcer. Aturan anti-abuse section 29 hidup di sini sebagai batas XP atas jendela 24 jam bergulir, per source sekaligus menyeluruh, yang dibaca dari baris durable lewat xp_earned_since dan bukan dari cache sehingga restart cache tidak dapat mereset batas seorang pelaku, dan currency tidak pernah diberikan berdasarkan jumlah pesan secara naif sebagaimana dilarang section 30. purchase sengaja tidak memeriksa kepemilikan lebih dulu, karena pemeriksaan seperti itu justru mematahkan idempotency yaitu retry yang sah akan dijawab ALREADY_EXISTS padahal pembelian pertamanya berhasil; alih-alih itu ia langsung mem-posting, dan store memeriksa idempotency key lebih dulu sehingga retry dengan key yang sama dijawab sebagai duplikat, lalu memvalidasi receipt entitlement sebelum penulisan atomik sehingga percobaan baru atas item yang sudah dimiliki dijawab ALREADY_EXISTS tanpa satu koin pun berpindah, dan satu strategi post-first itu memenuhi kedua kontrak sekaligus. send_gift adalah dua transaksi yang keduanya idempotent dan sengaja sanggup sembuh dari crash: transaksi pertama memungut pengirim dan mencatat gift dengan gift_id otoritatif diambil dari ref_id transaksi yang telah di-posting, lalu transaksi kedua memberi penerima reputasi gift dalam bentuk points, di-key pada gift_id itu dan diteruskan bukan ditelan sehingga retry menuntaskan langkah kedua yang mungkin belum sempat ditulis; gift kepada diri sendiri ditolak sebagai validasi, dan penerima diberi tahu lewat Announcer hanya bila transaksinya bukan duplikat supaya retry tidak berbunyi dua kali. Announcement yang diserahkan ke port itu sengaja kecil yaitu account_id, kind, dua Id opsional untuk actor dan subject, serta satu timestamp, tanpa field teks sama sekali karena payload push adalah wake-up dan bukan kalimat sesuai section 44 sehingga kata-katanya dipilih layer notifikasi dari kind-nya, dan kegagalan port dicatat lewat kode error saja tidak pernah lewat identitas siapa yang hendak diberi tahu lalu ditelan, sebab gift yang gagal berbunyi tetap gift yang sampai dan sudah dibayar. leaderboard di-cache untuk waktu singkat sesuai leaderboard_ttl_ms sehingga board yang ditonton ribuan orang menjadi satu pembacaan store per periode cache dan bukan satu per penonton, dan karena crate ini tidak memiliki serde maka codec-nya adalah baris big-endian selebar 32 byte tetap yang decode-nya mengembalikan None dan bukan error pada skew format sehingga nilai yang ditulis build lebih lama atau lebih baru dalam bentuk yang tidak dikenal cukup dihitung ulang dari store, dan tag cache-nya sengaja tidak memuat now maupun since supaya cache tetap kena lintas TTL. Karena opcode economy 160 sampai 162 masih SPEC, biaya tiap operasi client adalah konstanta lokal yang dipungut charge() mengikuti preseden migo-moderation dan bukan biaya yang digerakkan IDL, sedangkan grant, award, dan award_badge tidak memungut apa pun karena server yang memanggilnya, dan metode crate ini mengembalikan tipe domain yaitu Wallet, PurchaseOutcome, GiftOutcome, Vec<LedgerEntry>, ProgressionView, dan Vec<Rank> sementara layer 4 yang memasangkan opcode-nya, mengikuti preseden migo-social dan migo-notify. Berbeda dari saudara-saudaranya crate ini tidak menambah satu pun metode store maupun perubahan schema, karena seluruh aturan yang wajib selamat dari crash yaitu leg berjumlah nol, satu currency per transaksi, idempotency, lantai overdraft yang melarang akun User bernilai negatif sementara Mint, Fee, dan Escrow dikecualikan, serta batas harian XP sudah hidup di EconomyStore dan ProgressionStore, dan service ini tipis di atasnya yaitu menyusun akun, memberi harga dari Catalogue, dan menerjemahkan jawaban store. Tidak ada seri metrik berlabel SKU karena SKU tidak terbatas yaitu setiap tema musiman dan setiap item avatar terbatas mencetak satu dan meninggalkannya selamanya, sehingga belanja dilabeli Category yang tertutup pada tujuh dan gift dilabeli Gift yang tertutup pada sepuluh, dan tidak ada seri berlabel account maupun berlabel pasangan account karena penghitung yang berkunci pada pengirim menuju penerima adalah grafik pemberian gift yang dibangun ulang dari Prometheus yaitu grafik sosial yang justru dijauhkan section 174 dari endpoint metrik; pertanyaan pendapatan per item dibaca dari ledger yang memang tempatnya, sedangkan endpoint metrik menjawab seberapa banyak pemberian gift yang terjadi dan bukan siapa membeli item apa, dan 12 test yang menutup parse dan penolakan SKU beserta kategori dan slug-nya, kode gift sebagai SKU pada kategori gift, kurva level bersama inversinya tepat pada batas dan pada XP nol maupun negatif, penggabungan dan pengujian attribute, round trip reason dan source, jendela XP 24 jam yang bergulir dan terurut, sepuluh gift default yang lengkap dan berharga, listing yang mengganti dan bukan menduplikasi, mystery box sebagai listing berharga tetap seperti yang lain, serta tema musiman yang tetap terpisah dari gift.
migo-keys, yaitu trait Keyring dengan 2 metode yaitu publish dan bundles, dan keduanya adalah dua sisi dari satu hal yang sama: sebuah device menyatakan apa kunci publiknya, dan seorang pengirim menanyakan apa kunci publik orang lain. Keduanya berada pada satu trait karena satu aturan yang sama mengatur keduanya, yaitu server tidak pernah memegang kunci privat, tidak pernah menjamin kunci publik, dan tidak pernah membiarkan satu device berbicara untuk device lain, sedangkan trait yang dapat menyajikan bundle tanpa menjadi trait yang memverifikasi tanda tangan di atasnya adalah separuh operasi tanpa pemeriksaannya. Tidak ada satu pun parameter maupun field balikan yang dapat ditempati kunci privat, dan itulah cara larangan section 163 ditegakkan alih-alih hanya diingat: perubahan yang hendak menitipkan kunci privat harus mengubah berkas trait itu sendiri, yaitu perubahan yang dilihat reviewer. request publish juga tidak memiliki field device, sehingga material selalu difile di bawah Caller::device_id dan satu device tidak dapat mempublikasikan identity untuk device lain, yang justru akan menjadi keseluruhan serangannya karena identity yang dipublikasikan adalah yang diverifikasi setiap pengirim di kemudian hari. publish bersifat replace dan bukan merge, termasuk bagi one-time prekey, karena hal yang tidak boleh terjadi adalah server membagikan prekey yang separuh privatnya tidak lagi dipegang device: client yang dipasang ulang telah kehilangan setiap kunci privat lamanya, dan merge akan membuat server menyajikan kunci-kunci itu selama berminggu-minggu dengan setiap session yang terbentuk darinya tidak dapat didekripsi penerimanya, sehingga top-up berarti mempublikasikan batch baru di bawah key id baru sedangkan client menahan separuh privat batch yang baru saja dipensiunkannya sampai pesan yang sedang di jalan mendarat. Verifikasi tanda tangan di server bukan yang membuat protokol aman, sebab pemeriksaan yang menentukan dilakukan pengirim di device sebelum ia menyusun apa pun dan itulah yang membuat server yang menukar prekey dengan miliknya sendiri gagal alih-alih berhasil; server memeriksa juga pada saat publikasi karena bundle rusak yang tersimpan adalah percakapan yang tidak dapat dimulai tanpa memberi alasan, dan menolaknya di sana mengubah pesan-ke-device-ini-gagal-diam-diam menjadi INVALID_KEY_MATERIAL yang diterima client tepat ketika ia publish, yaitu laporan bug alih-alih misteri. Itu adalah pemeriksaan integritas data yang dilakukan pihak yang tidak dipercaya siapa pun, dan layak ada persis karena alasan itu dan tidak lebih. Panjang yang salah dijawab VALIDATION_FAILED dan bukan INVALID_KEY_MATERIAL, karena kunci 31 byte adalah client yang menyusun frame-nya salah dan bukan client yang kriptografinya salah, dan angka panjangnya boleh masuk ke pesan sebab panjang bukan material kunci sekaligus satu-satunya angka yang mengubah server-menolak-kunci-saya menjadi bug yang dapat diperbaiki. Key id di paruh atas rentang u32 ditolak alih-alih dibiarkan melipat, sebab id yang melipat adalah prekey yang menurut client dipublikasikan di bawah satu angka sedangkan server menyajikannya di bawah angka lain. Id yang terduplikasi di dalam satu publikasi dilewati dan bukan menggagalkan publikasinya, supaya client dengan satu id kembar masih dapat mempublikasikan sisanya. Satu one-time prekey dihabiskan per bundle yang dikembalikan dan itu memang inti operasinya alih-alih efek samping, dikerjakan atomik di dalam satu panggilan store sehingga tidak ada read-then-write dan tidak ada lock, sebab satu prekey yang sama yang diberikan kepada dua pengirim menurunkan jaminan keduanya menjadi hanya signed prekey tanpa seorang pun dari keduanya akan pernah mengetahuinya. Ketika sebuah device tidak punya sisa, bundle tetap kembali tanpa prekey dan Fetched::any_exhausted menyatakannya, karena percakapan yang dimulai dengan forward secrecy sedikit lebih lemah pada pesan pertamanya lebih baik daripada percakapan yang tidak dapat dimulai sedangkan device pemiliknya diberi tahu untuk mempublikasikan lagi, dan deployment yang lebih memilih gagal menyatakannya lewat refuse_when_exhausted yang menjawab PREKEYS_EXHAUSTED. Material kunci device yang dicabut tidak pernah dikembalikan karena store menerapkan filter itu di dalam query dan bukan sesudahnya, sehingga bukan sesuatu yang crate ini dapat lupa mengecualikan, dan karena itu crate ini tidak memerlukan DeviceStore sama sekali. Tidak ada pemeriksaan block dan tidak ada gerbang privasi pada bundles, sebab kunci publik memang publik, kiriman pesannya sendiri sudah digerbang migo-social, dan satu-satunya yang bocor adalah bahwa sebuah akun pernah mempublikasikan kunci, yang benar bagi setiap akun. Urutan pemungutan biaya sengaja tidak simetris: publish memungut sesudah tulisan store berhasil karena publikasi yang ditolak store tidak layak menghabiskan anggaran dua puluh kali harga satu fetch sedangkan store menolak permintaan yang sama itu setiap kali sehingga jalur tanpa pungutan tidak mencapai apa pun, sedangkan bundles memungut sebelum pembacaan karena pembacaan itu sendiri adalah efek sampingnya. Bucket rate limit adalah endpoint per akun ditambah akun, dan sengaja bukan device, karena anggaran per device akan membuat akun dengan empat puluh device dapat mempublikasikan empat puluh kali lebih sering sedangkan churn kunci adalah urusan tingkat akun. Section 163 menuntut signed_prekey_expires_at sedangkan IDL beserta golden vector-nya sudah beku tanpa field itu, maka jalan tengahnya adalah tipe domain membawanya, composition root sebagai satu-satunya tempat yang tahu jam mengisinya dengan waktu sekarang ditambah masa hidup signed prekey yang ditetapkan crate keys yaitu tiga puluh hari, proyeksi wire membuangnya, dan aturan menolak prekey yang sudah kedaluwarsa pada saat publikasi ditegakkan terhadap field itu sehingga ia mulai berlaku tanpa biaya tambahan ketika IDL menyusul; separuh yang lebih aman dari ketidaksepakatan itu adalah kedaluwarsa yang dipilih server, sebab client yang memilih sendiri dapat memilih sepuluh tahun ke depan. Tidak ada revocation di sini karena mencabut material kunci adalah satu langkah dari menghapus device dan bukan operasi yang dilakukan client atas kemauannya sendiri, sehingga KeyStore::revoke_device_keys dipanggil oleh siapa pun yang menghapus device dan metode revoke di trait ini akan menjadi cara kedua untuk melakukan separuhnya, dengan separuh yang melewatkan pembongkaran session meninggalkan device yang tidak dapat ditulisi tetapi masih login. Tidak ada group epoch dan tidak ada call key karena section 163 menandai keduanya SPEC, epoch sender key adalah urusan room, dan kunci media panggilan diturunkan di device dari session yang tidak pernah dilihat crate ini. Tidak ada verifikasi fingerprint: server mengembalikan fingerprint dari apa yang ia simpan, sedangkan memutuskan bahwa sebuah fingerprint adalah yang benar adalah hal yang dilakukan dua manusia di luar jalur, dan bit verified di sisi server akan menjadi server yang menegaskan satu-satunya fakta yang justru tidak boleh dipercaya darinya. Crate ini tidak memiliki jam sendiri, sehingga setiap waktu masuk lewat Caller::now dan test dapat memajukan waktu tanpa menunggu. Tidak ada seri metrik berlabel akun maupun device, hanya rasio agregat yaitu served per asked dan exhausted per served, sesuai section 174, dan 34 test yang menutup penolakan caller tanpa identitas sebelum satu pun biaya dipungut, panjang yang salah pada signed prekey beserta tanda tangannya dan pada one-time prekey yang dijawab VALIDATION_FAILED dengan nama field beserta kedua angkanya, identity key yang bukan 64 byte dan paruh exchange bernilai order kecil yang keduanya dijawab INVALID_KEY_MATERIAL, tanda tangan yang dibuat identity lain, tanda tangan sah yang dipindahkan ke key id berbeda sehingga pengikatan key id ke dalam tanda tangan terbukti, prekey yang kedaluwarsa tepat pada Caller::now yang ditolak sedangkan satu milidetik sesudahnya diterima, key id di luar rentang i32 positif yang ditolak alih-alih dibiarkan melipat sementara i32::MAX diterima, id kembar yang dilewati dan bukan menggagalkan publikasinya, publikasi yang replace dan bukan merge, dua device satu akun yang masing-masing menyimpan identity-nya sendiri sehingga penempatan di bawah Caller::device_id terbukti tanpa perlu field device pada request-nya, device yang tidak dikenal yang dijawab NOT_FOUND tanpa satu pun pungutan, kedua urutan pungutan yang tidak simetris yaitu publish sesudah tulisan dan bundles sebelum pembacaan yang dibuktikan lewat fetch yang ditolak limiter dan tidak menghabiskan satu prekey pun, bucket yang tepat yaitu endpoint per akun ditambah akun dan bukan device dengan harga yang diambil dari IDL, fingerprint 64 hex huruf kecil yang mengikuti identity dan bukan publikasinya, satu prekey per fetch yang tidak pernah diberikan dua kali, bundle tanpa prekey yang signed prekey-nya tetap utuh beserta any_exhausted dan PREKEYS_EXHAUSTED yang hanya muncul di bawah refuse_when_exhausted, ambang low water yang dipakai client untuk memutuskan kapan mempublikasikan batch baru, fanout satu bundle per device hidup, material device yang dicabut yang tidak pernah disajikan lewat kedua jalur fetch, device yang lebih banyak daripada batas bundle per fetch yang tetap dilayani dan bukan ditolak, bundle tersaji yang identity dan tanda tangannya diverifikasi ulang persis seperti yang dilakukan pengirim sebelum menyusun apa pun, ketujuh penghitung metrik beserta kelima seri alasan penolakan yang terdaftar dari nol dan tidak satu pun berlabel akun maupun device, serta penolakan yang tidak membawa material kunci yang ditolaknya.
migo-rooms, yaitu 15 metode pada trait Roomkeeper untuk pembuatan room, join, leave, daftar, roster, pengubahan setelan, archive, penetapan role, grant dan deny per anggota, sanksi, alih kepemilikan, dan authorize. Seluruh 19 permission produk section 48 dihitung di satu fungsi permission::resolve dengan satu urutan presedensi yaitu default role, ditambah grant, dikurangi deny, karena jumlah tempat yang masuk akal ingin memeriksa sebuah permission cukup banyak sehingga implementasi kedua akan muncul dalam satu release. Domain lain tidak membaca tabel permission melainkan memanggil authorize, karena dua crate layer 3 tidak boleh saling bergantung, dan Authorized membawa kembali conversation id, jenis room, mask efektif pemanggil, dan interval slow mode yang berlaku bagi pemanggil itu sehingga pemanggil tidak perlu mencari room-nya lagi dan tidak perlu menulis ulang pengecualian moderator. Setiap metode yang mengubah sesuatu mengembalikan rencana Fanout dan bukan mengirimkan apa pun, dan None adalah section 156 di dalam type system, yaitu join ke room yang sudah dimasuki, layar setelan yang dikirim tanpa perubahan, dan role yang diset ke nilai yang sudah dipegang semuanya tidak menghasilkan frame. Tiga hal yang sengaja tidak dilakukan di sini: online_count keluar sebagai nol karena angka itu adalah ukuran himpunan subscriber yang dipegang gateway dan menghitungnya di sini berarti membaca roster 800 baris pada setiap subscribe yang justru dilarang section 14; slow mode dilaporkan tetapi tidak ditegakkan karena timestamp kirim terakhir hidup bersama pesan; dan service::open tidak punya parameter cache sama sekali karena baris membership adalah yang memutuskan siapa boleh bicara, sehingga salinan basi berarti anggota yang dibanned dua menit lalu masih berbicara. Alasan ban yang ditulis moderator tidak pernah dibawa ke dalam error, karena teks itu ditulis untuk moderator berikutnya sedangkan error dibaca oleh orang yang dibanned. Satu room memiliki tepat satu sequencer di home region-nya sesuai section 54, dan partisi dijawab ROOM_READ_ONLY_PARTITION dan bukan dengan memilih sequencer kedua, dan 108 test yang menutup penolakan caller tanpa identitas pada setiap metode sebelum satu pun biaya dipungut, namespace slug yaitu huruf kecil dengan panjang terbatas dan tanda hubung yang tidak pernah di tepi serta slug yang tidak boleh berupa id supaya satu ruang nama tidak memiliki dua bentuk, pembuatan room yang menjadikan pembuatnya owner sekaligus anggota pertama dengan slug terpakai dijawab ALREADY_EXISTS dan topic yang seluruhnya spasi menjadi tanpa topic dan nama diukur per karakter dan bukan per byte serta seratus permintaan cacat yang tidak menghabiskan anggaran satu pun pembuatan yang sah sesudahnya, join room terbuka yang mengabarkan room sekaligus join kedua yang tidak menghasilkan frame dan rejoin yang mempertahankan role beserta mute yang ditinggalkannya dan ban yang tetap berlaku sesudah keluar lalu kembali dan invite code beserta approval queue yang dijawab FEATURE_DISABLED alih-alih diabaikan diam-diam dan perubahan policy yang tidak mengeluarkan orang yang sudah di dalam dan room penuh dijawab ROOM_FULL dan room yang diarsipkan dijawab ROOM_ARCHIVED, leave yang mengabarkan room sedangkan owner dijawab CONFLICT karena room tanpa owner adalah room tanpa siapa pun yang dapat memperbaikinya, daftar yang berperingkat menurut jumlah anggota lalu waktu pembuatan dengan room arsip disembunyikan dan pencarian yang mencocokkan nama maupun slug tanpa peka huruf dan page size yang diklem di kedua arah dan filter yang belum didukung build ini dijawab FEATURE_DISABLED alih-alih dijalankan sebagian dan query terlalu panjang dijawab FIELD_TOO_LONG, summary yang melaporkan role pemanggil sendiri sedangkan role orang yang keluar atau dibanned tidak dilaporkan, resolve yang menerima slug maupun teks id kanonik dan menjawab NOT_FOUND alih-alih menyebut bentuk input yang salah supaya slug tidak menjadi oracle keberadaan room, alias yang dibagikan yang sengaja bukan kunci lookup, roster yang hanya untuk anggota dengan orang yang keluar disembunyikan dan anggota yang dibanned tidak dapat membacanya, setelan yaitu rename yang tidak menghasilkan frame sedangkan topic disiarkan dengan penghapusannya berupa string kosong dan slow mode disiarkan dalam milidetik dengan nol saat dimatikan dan layar setelan yang tidak mengubah apa pun tidak menulis apa pun termasuk tidak memajukan waktu ubah, edit yang menuntut bit edit sedangkan perubahan policy menuntut bit manage juga, archive yang hanya boleh dilakukan owner sehingga Manager dengan seluruh bit pun ditolak dan yang kedua kalinya bukan error, algebra permission yaitu setiap role membawa seluruh milik role di bawahnya dan deny menang serta menang terakhir dan bit grant yang tidak dikenal tidak memberi apa pun dan mute menahan bicara tanpa menahan satu pun hal yang dibutuhkan pendengar, authorize yang menamai keempat penolakannya yaitu NOT_A_MEMBER dan BANNED dan MUTED dan PERMISSION_DENIED dengan ban terlihat lebih dulu daripada kepergian karena store menstempel kepergian saat membanned dan moderator tidak pernah dikenai slow mode dan mask kosong hanya menanyakan keanggotaan dan enam puluh panggilan authorize yang tidak dipungut biaya karena ia dipanggil di dalam operasi yang sudah dipungut, role yaitu promosi yang disiarkan sebagai anggota yang masih ada dan Owner yang bukan role yang dapat diberikan dan aktor yang harus mengalahkan role sekarang maupun role yang diberikan dan anggota yang sudah keluar yang tidak dapat dipromosikan, grant dan deny per anggota yang disimpan tanpa pernah disiarkan dengan bit yang tidak didefinisikan build ini beserta grant dan deny yang bertumpang tindih dijawab VALIDATION_FAILED dan permission yang tidak dapat diberikan oleh orang yang sendirinya tidak memegangnya, sanksi yaitu mute yang tidak disiarkan karena teguran bukan hukuman dan unmute yang tidak mengangkat ban sebagaimana mute tidak mengangkatnya dan kick maupun ban yang menyiarkan tanpa socket moderator yang melakukannya sehingga id yang dikecualikan terbukti device dan bukan akun dan ban tanpa masa berlaku berupa timestamp jauh di depan dan bukan null dan durasi absurd yang diklem alih-alih melipat dan setiap sanksi yang menuntut permissionnya sendiri sehingga Helper dapat mute tetapi tidak kick maupun ban dan keluar lebih dulu yang bukan cara menghindari ban serta owner yang tidak dapat disanksi siapa pun, alih kepemilikan yaitu REAUTHENTICATION_REQUIRED yang diperiksa sebelum limiter sehingga empat puluh penolakan tidak menghabiskan anggaran owner yang sebenarnya dan hanya owner yang boleh melakukannya dan hanya kepada anggota aktif dan ke diri sendiri tidak mengubah apa pun dan owner lama turun menjadi Manager alih-alih dikeluarkan supaya alih yang disesali masih dapat dibatalkan serta tepat satu event tentang owner baru karena dua event akan tiba dalam urutan yang tidak dijanjikan gateway, dan seluruh seri metrik yang sudah ada bernilai nol sebelum apa pun terjadi supaya dashboard menunjukkan nol alih-alih lubang serta tidak satu pun label yang memuat akun, device, atau room sesuai section 174.
migo-social, yaitu 19 metode pada trait Graph untuk permintaan pertemanan, jawaban, penghapusan, follow, block, favourite, keenam daftar yang menyertainya, standing, may_interact, suggest, search, dan profiles. Satu metode yang dipanggil domain lain adalah may_interact, yaitu messaging sebelum kiriman direct, call sebelum panggilan berdering, dan presence sebelum mengungkapkan last seen, sehingga aturan bahwa block bersifat simetris dan permintaan yang masih menggantung bukan pertemanan ditulis tepat satu kali. Penolakan dijawab PRIVACY_RESTRICTED baik untuk block milik subject maupun untuk setiap penolakan privasi, sedangkan BLOCKED_BY_USER hanya dipakai untuk block milik pemanggil sendiri karena memberi tahu seseorang apa yang ia sendiri lakukan tidak membocorkan apa pun, dan itulah yang dituntut section 180. Karena itu Standing tidak memiliki field blocked_by. Pencarian mutual friend dibatasi 200 per sisi dan jawaban yang tidak lengkap diperlakukan sebagai tidak ada mutual friend, yaitu penolakan dan bukan izin, karena gerbang privasi yang gagal terbuka ketika datanya membesar berarti gerbang yang berhenti bekerja justru bagi akun yang paling banyak dipertaruhkan. Interaction berisi empat nilai dan bukan tujuh seperti daftar section 124 karena profile hanya memiliki tiga kolom visibility, dan variant kelima akan menjadi gerbang yang selalu menjawab Everyone, yaitu sesuatu yang tampak seperti kontrol privasi di API dan berperilaku seperti no-op. Call mengambil nilai paling ketat antara call_default milik deployment dan who_can_message milik akun karena tidak ada kolom who_can_call, sehingga default Friends pada section 180 dihormati tanpa mengarang kolom dan orang yang menyetel pesan ke Nobody juga menolak panggilan. profiles adalah satu-satunya metode crate ini yang sudah memiliki handler opcode, yaitu PROFILE_FETCH bernomor 112, dan ia menghilangkan secara diam-diam alih-alih menolak: subject yang memblokir pemanggil atau yang diblokir pemanggil, yang tidak memiliki baris profile, maupun yang tidak memiliki baris account tidak muncul pada jawaban tanpa satu pun error, sehingga pemanggil tidak dapat membedakan memblokir-kamu dari terhapus dari tidak pernah ada, persis yang dituntut section 180. Karena itu jawabannya boleh lebih pendek daripada permintaannya dan tidak berurutan, dan client mencocokkannya lewat user_id dan bukan lewat posisi. Id pemanggil sendiri melewati pemeriksaan block alih-alih ditangani khusus di tempat lain, sebab tidak seorang pun dapat memblokir dirinya sendiri sedangkan cabang hasil-nil bagi kasus diri sendiri adalah cabang yang harus tetap benar selamanya. Deduplikasi dilakukan sebelum pemungutan biaya dan bukan sesudahnya supaya batas 64 id per batch dan harga rata itu berlaku atas pekerjaan yang benar-benar dilakukan, sebab client yang mengirim id yang sama enam puluh empat kali mendapat satu profile dan pantas membayar satu batch. Karena opcode 113 sampai 117 masih SPEC, metode yang mengubah sesuatu mengembalikan Notice di atas NOTIFICATION_EVENT opcode 144 dengan title dan body dibiarkan kosong supaya client menulis kalimatnya dalam bahasa pembaca. Satu penyimpangan store yang disengaja: SocialStore::count_relationships, karena tanpa hitungan, ceiling pertemanan hanya dapat ditegakkan dengan membaca seluruh daftar pada setiap permintaan, dan 111 test yang menutup penolakan caller tanpa identitas pada seluruh 19 metode sebelum satu pun biaya dipungut serta akun yang tidak boleh berelasi dengan dirinya sendiri dan subject yang tidak diisi dijawab FIELD_REQUIRED, permintaan pertemanan yang menulis kedua belah baris sekaligus mengabari pihak lain dan permintaan kedua yang menjawab hal yang sama tanpa berbunyi lagi dan permintaan yang bersilangan yang langsung menerima permintaan yang sudah menunggu serta pertemanan yang sudah ada dijawab sebelum satu pun setelan dibaca, block milik pemanggil yang disebut namanya sedangkan block milik subject tidak dapat dibedakan dari penolakan setelan padahal penghitungnya tetap membedakan keduanya, kebijakan tambah Friends yang berarti teman dari teman sedangkan kebijakan Nobody menolak bahkan teman dari teman, permintaan yang belum dijawab yang bukan pertemanan bagi gerbang mana pun dan baris pertemanan tanpa tanggal persetujuan yang tidak membuka apa pun, ceiling pada kedua daftar teman dan dua puluh percobaan permintaan per anggaran, jawaban yang menulis kedua pertemanan sambil mempertahankan tanggal permintaannya dan penolakan yang senyap serta boleh diminta ulang dan hanya akun yang dituju yang boleh menjawabnya dan block yang ditulis saat permintaan menunggu yang menang lalu membersihkannya sedangkan setelan yang dipersempit tidak membatalkan permintaan yang sudah menunggu, pemutusan pertemanan yang membersihkan kedua sisi tanpa suara, follow yang satu arah tanpa persetujuan dan ditolak pada kedua arah block, block yang membatalkan pertemanan permintaan follow dan favourite sekaligus menghitung tepat yang benar-benar dihapus sehingga block atas orang asing tidak melaporkan penghapusan apa pun, favourite yang merupakan penanda pribadi yang tidak terlihat siapa pun, keenam daftar yang dibaca dari ujung yang benar dan seluruhnya terbaru lebih dulu dengan page size diklem di kedua arah dan tidak pernah menampilkan grafik milik orang lain, standing yang melaporkan tujuh fakta dari sisi pemanggil tanpa pernah melaporkan block milik subject dan memiliki anggaran endpoint-nya sendiri, gerbang yang membaca kolomnya masing-masing dengan call mengambil nilai paling ketat dan tidak pernah dipungut biaya serta pencarian mutual friend yang tidak dapat diselesaikan yang menolak alih-alih mengizinkan, saran yang menghitung teman bersama dan berhenti pada satu hop dan tidak pernah menawarkan teman maupun permintaan yang menggantung maupun diri sendiri maupun block dari kedua arah dengan jumlah daftar teman yang dibaca per putaran terbukti dari penghitungnya, pencarian yang mencocokkan awalan username dan potongan display name tanpa peka huruf dan menyembunyikan kedua arah block serta akun yang tidak ingin ditemukan maupun akun yang disuspend sedangkan query cacat tidak menghabiskan anggaran, batch profile yang menampilkan tujuh field publik dengan id kembar menjadi satu kartu dan satu biaya dan id yang diblokir tidak dapat dibedakan dari id yang tidak pernah ada sementara kartu milik pemanggil sendiri selalu disajikan, block yang tidak menjalar melewati dua akun di dalamnya, serta seluruh seri metrik yang bernilai nol sebelum apa pun terjadi dan tidak satu pun label yang memuat akun, device, atau username sesuai section 174. Suite ini menangkap tiga bug produk sebelum ada yang memakainya: penghitung gerbang memasukkan block milik subject ke keranjang restricted sehingga operator kehilangan satu-satunya bacaan yang justru tidak boleh dimiliki pemanggil, projeksi Edge menandai baris permintaan yang belum dijawab sebagai sudah disetujui sehingga Graph::pending akan mengabarkan client bahwa permintaan yang menggantung adalah pertemanan, dan block menghapus pertemanan follow dan favourite tanpa menghitungnya sehingga selisih antara edge yang ditambahkan dan yang dihapus melenceng tepat sebanyak jumlah block.
migo-media, yaitu trait Library dengan 8 metode untuk begin, status, commit, abort, fetch_url, describe, delete, dan record_scan, ditambah dua trait port yaitu Storage dengan 5 metode dan ScanQueue dengan 2 metode yang diimplementasikan oleh composition root sehingga crate ini tidak pernah menaut SDK S3 mana pun. Byte tidak pernah menyentuh proses ini sesuai section 168, yaitu client meminta izin, menerima signed URL berumur pendek dan sebuah ticket, mendorong byte-nya langsung ke object storage, lalu kembali untuk commit. Ticket adalah token kapabilitas ber-MAC yang stateless sehingga commit tidak memerlukan baris upload sementara yang harus disapu bila client menghilang. Pada commit ukuran diverifikasi terhadap storage dan jenis isi ditentukan dengan membaca magic byte, karena Content-Type dari client tidak pernah dipercaya sesuai section 122. Media dengan scan_status pending tidak pernah dilayani kepada user lain sesuai section 168, dan media room Public serta Managed wajib lulus pemindaian sedangkan media percakapan private tidak dapat dipindai server karena yang dilihat server hanyalah ciphertext. Satu penyimpangan store yang disengaja dan wajib dibaca bersama section 168: kolom baru media_object.conversation_id yang nullable beserta index media_conversation_idx, karena section 168 mengharuskan otorisasi download dijawab dengan pertanyaan apakah pemohon adalah anggota conversation atau room yang memuat media itu, sementara tautan antara objek dan conversation-nya untuk media terenkripsi berada di dalam ciphertext sehingga server tidak akan pernah melihatnya. Nilai NULL berarti media profile yang boleh dirender setiap akun terautentikasi. Kolom itu dicatat pada begin, yaitu satu-satunya saat server pernah diberi tahu, dan bukan diambil dari conversation yang disebut client pada saat download, karena pemeriksaan seperti itu akan berjalan, lulus, dan tidak berarti apa-apa. Signed URL tidak pernah ditulis ke log, metrik, error, maupun analytics karena URL itu sendiri adalah kredensial sesuai section 69, dan tidak ada histogram ukuran objek karena satu bucket yang berisi satu observasi adalah satu file milik satu orang, dan 50 test yang menuntut bahwa tipe sebuah objek datang dari byte-nya dan bukan dari header yang dikirim client, bahwa satu tiket terikat pada satu akun dan satu device sehingga tiket yang tanda tangannya tidak terverifikasi tidak dapat dibedakan dari tiket milik device yang salah, bahwa objek yang tidak boleh dilihat pemanggil dan objek yang tidak ada menerima jawaban yang sama persis, bahwa setiap plafon per jenis diterima tepat pada angkanya dan ditolak satu byte di atasnya, bahwa anggaran laju milik akun dan bukan milik device sehingga device kedua tidak melipatgandakan kuota unggah seseorang sesuai section 70, dan bahwa tidak satu pun signed URL, storage key, atau id pernah muncul sebagai label metrik. Suite ini menemukan tiga cacat nyata yang seluruhnya sudah diperbaiki dalam commit yang sama: crate ini sama sekali tidak memeriksa identitas pemanggil, sehingga permintaan tanpa akun dan tanpa device dipungut dari satu bucket yang dibagi bersama oleh setiap permintaan tanpa identitas di deployment itu dan, untuk tujuan profile yang memang tidak menanyakan keanggotaan apa pun, tetap keluar membawa signed URL beserta tiket bertanda tangan untuk storage key yang sungguh ada; lebar, tinggi, dan durasi yang dideklarasikan client diperiksa terhadap plafon deployment pada begin lalu dibuang tanpa pernah sampai ke baris database, sehingga pemeriksaan durasi voice note hanya menjadi hiasan dan format tiket dinaikkan ke versi dua supaya angka yang diterima begin adalah angka yang ditulis commit; dan commit yang diulang karena jawaban pertamanya tidak pernah sampai ke client ditolak sebagai objek yang sudah ada, sehingga client kehilangan id dari byte yang sudah berhasil diunggahnya, padahal id itu dicetak pada begin justru supaya commit dapat diulang tanpa melahirkan objek kedua dari satu unggahan.
migo-moderation, yaitu trait Warden dengan 7 metode untuk pengiriman laporan, pembacaan queue, pembacaan satu laporan, penyelesaian laporan, aksi moderator, pembacaan audit trail, dan penilaian abuse, ditambah trait port Roster dengan 1 metode. Roster ada karena schema tidak memiliki kolom role global, yaitu docs/04-data-model.md hanya memberi role kepada anggota room dan tidak kepada siapa pun selain itu, sehingga siapa yang berstatus staff adalah keputusan deployment dan bukan tabel yang diarang crate ini, dengan bentuk yang sama seperti Storage pada migo-media. Pemanggil dibagi menjadi dua tipe terpisah yaitu Caller yang mengirim laporan dan Operator yang menindaklanjutinya, bukan satu tipe dengan flag, supaya compiler yang menemukan ketika request user biasa mencapai jalur yang men-suspend akun, yaitu pemeriksaan yang tidak berbiaya saat runtime dan tidak bergantung pada seseorang ingat menuliskannya. Powers adalah bitmask empat bit TRIAGE, TAKEDOWN, SUSPEND, dan AUDIT yang diselesaikan lewat Roster pada setiap panggilan dan tidak pernah dibaca dari request. Sebuah laporan adalah pointer dan bukan salinan, sesuai komentar schema bahwa evidence adalah referensi dan bukan salinan isi pesan, karena menyalin ciphertext private ke tabel moderation akan meniadakan gunanya mengenkripsi pesan itu. Permintaan satu laporan oleh pemanggil tanpa power dijawab NOT_FOUND dan bukan PERMISSION_DENIED sesuai section 48, karena pertanyaan apakah ada laporan tentang sebuah akun adalah pertanyaan yang jawabannya berharga bagi orang yang salah. Setiap aksi menuntut faktor yang baru dibuktikan, dan pemeriksaannya diletakkan sesudah pemeriksaan power tetapi sebelum rate limiter, supaya pemanggil yang bukan staff tidak belajar bahwa aturan kesegaran itu ada dan supaya penolakan tidak pernah sempat memungut budget seorang moderator. Baris audit ditulis pada panggilan yang sama dengan aksinya dan kegagalannya diteruskan sebagai error, yang justru berbeda dari migo-auth yang mencatat kegagalan audit lalu melanjutkan: itu benar di sana dan salah di sini, karena sign in yang tidak tercatat adalah lubang pada sebuah riwayat sedangkan suspend yang tidak tercatat adalah akun yang pemiliknya tidak dapat mengetahui siapa yang menutupnya. Peringatan tidak menulis tabel lain sama sekali karena audit_for_target atas akun itu sendiri adalah riwayat peringatan, sudah terindeks dan sudah terurut terbaru lebih dulu, sedangkan tabel terpisah akan menjadi riwayat kedua yang menyimpang dari yang pertama. Tiga dari enam kategori deteksi otomatis pada section 49, yaitu scam, malicious link, dan abusive behaviour, tidak dapat dideteksi di server sama sekali karena ketiganya adalah penilaian atas isi pesan yang bagi percakapan private hanya berupa ciphertext sesuai section 122, sehingga yang tersisa hanyalah laju dan bentuk, dan itulah isi Signals. Adaptive rate limiting yang diminta section 50 diwujudkan sebagai score menjadi Risk menjadi Risk::clamp atas trust tier, yang hanya pernah menurunkan tier dan tidak pernah menaikkannya, sehingga crate ini tidak pernah memiliki bucket apa pun. Sinyal terberat adalah jumlah laporan dari orang lain karena setiap satuannya berasal dari seseorang yang menekan tombol. auto_suspend default false karena sistem otomatis yang men-suspend akun berdasarkan skor metadata akan men-suspend akun seseorang berdasarkan skor metadata, dan bila deployment menyalakannya, suspend selalu untuk periode terbatas dan dicatat dengan AuditActorKind::System tanpa actor_id, karena tidak ada orang yang memutuskannya dan menyebut nama moderator yang sedang bertugas akan menautkan nama seseorang pada keputusan yang dibuat sebuah fungsi. Teks bebas yang ditulis operator hanya berada di audit_entry.reason dan tidak pernah masuk ke error, label metrik, notifikasi, maupun log. Tidak ada seri metrik berlabel reporter maupun operator, karena laporan per reporter dan aksi per operator keduanya adalah daftar nama orang yang dipublikasikan pada endpoint tanpa autentikasi, dan skor abuse bukan histogram karena bucket yang berisi satu observasi adalah skor satu orang. Ban dan mute per room tetap milik migo-rooms lewat set_room_sanction dan tidak dipindahkan ke sini, sedangkan remedi room di crate ini adalah archive atau eskalasi. Dua penyimpangan store yang disengaja: SafetyStore::open_report_by_reporter, karena idempotency yang diminta section 153 untuk sebuah laporan adalah pasangan reporter dan subject selama laporannya masih terbuka, dan tanpa metode itu satu-satunya cara mengenali pengulangan adalah memindai queue yang panjangnya terbatas sehingga di luar batas itu setiap pengulangan menjadi baris baru dan sebuah script dapat mengubah satu keluhan menjadi seratus ribu baris; dan SafetyStore::count_reports_about di atas report_subject_idx yang sudah ada, yaitu satu-satunya sinyal abuse di schema yang bukan penghitung laju dan bukan isi pesan, dikembalikan sebagai angka dan bukan sebagai baris karena barisnya akan membawa identitas para pelapor kepada siapa pun yang bertanya. Satu penyimpangan schema yang disengaja: nilai report.subject_kind = 4 untuk laporan bot, karena section 49 mendaftarkan laporan bot di samping laporan user, message, dan room, sedangkan melipatnya menjadi laporan user akan membuat moderator yang membaca queue tidak dapat membedakan orang yang kasar dari integrasi yang rusak, yaitu dua masalah berbeda dengan penyelesaian berbeda. Satu aksi yang belum ada dan alasannya sudah dicatat di kode: penonaktifan bot, karena belum ada store trait yang menjangkau kolom bot.disabled_at, dan variant DisableBot ditambahkan pada saat BotStore lahir, dan 84 test yang menuntut bahwa laporan dari caller tanpa akun maupun tanpa device ditolak sebelum satu pun biaya dipungut, bahwa power selalu diselesaikan lewat Roster pada setiap panggilan sehingga Operator yang membawa Powers::ALL di dalam request tidak memperoleh apa pun, bahwa permintaan satu laporan oleh pemanggil tanpa power tidak dapat dibedakan dari laporan yang tidak ada, bahwa setiap aksi menuntut faktor yang baru dibuktikan dan penolakannya terjadi sebelum rate limiter memungut budget seorang moderator, bahwa laporan atas satu pesan menyimpan message_id pada subject_id dan tidak pernah menyimpan conversation_id di sana sedangkan evidence tetap berupa id, bahwa keluhan yang sama dari pelapor yang sama selama laporannya masih terbuka adalah satu baris yang dijawab duplicate tanpa audit kedua dan tanpa hitungan kedua terhadap subject, bahwa queue selalu tertua lebih dulu dan dibatasi MAX_PAGE meskipun diminta u16::MAX serta jatuh ke ukuran halaman default ketika tidak diminta apa pun, bahwa takedown tidak melahirkan Notice sedangkan warn, suspend, dan reinstate melahirkannya, bahwa teks bebas yang ditulis operator hanya sampai ke audit trail dan tidak pernah ke Notice, metrik, maupun error, bahwa score menjadi Risk menjadi clamp yang hanya pernah menurunkan trust tier pada keempat tingkat terhadap kelima tier sehingga skor tidak pernah menjadi cara memperoleh tier yang lebih baik dan bot tetap bot di bawah Throttle, bahwa laporan yang lebih tua daripada jendela laporan tujuh hari berhenti dihitung, dan bahwa tidak satu pun id maupun username muncul sebagai label metrik. Suite ini menemukan dua cacat nyata yang keduanya sudah diperbaiki: file_report menerima caller yang membawa akun tanpa device sehingga sebuah laporan dapat ditulis tanpa device yang dapat dipertanggungjawabkan; dan backend in-memory mengurutkan queue menurut urutan penulisan baris sedangkan backend PostgreSQL mengurutkannya menurut created_at lalu report_id, yaitu selisih yang membuat setiap test yang mengandalkan urutan queue membuktikan urutan yang tidak dimiliki store sungguhan.
migo-notify, yaitu trait Notifier dengan 8 metode untuk notify, notify_many, inbox, badge, acknowledge, register, unregister, dan sweep, ditambah trait port PushSender dengan 2 metode yang diimplementasikan composition root sehingga crate ini tidak pernah menaut SDK FCM maupun APNs, dengan bentuk yang sama seperti Storage pada migo-media dan Roster pada migo-moderation, dan NoPush adalah implementasinya bagi deployment tanpa layanan push. Larangan section 44 bahwa payload push tidak boleh memuat plaintext pesan, plaintext audio voice note, maupun isi signaling tidak ditulis sebagai komentar melainkan sebagai tipe: Wakeup hanya memiliki kind, dua Id opsional, badge berupa u32, dan satu timestamp, sehingga tidak ada field String dan tidak ada byte array yang dapat memuat kalimat, dan satu-satunya teks yang keluar dari crate ini berasal dari Wakeup::alert yang mengembalikan &'static str dari match atas kind, yaitu 15 kalimat tetap yang dipilih pada saat kompilasi. Karena itu penulis yang ingin memasukkan preview pesan ke dalam push harus menambahkan field pada struct publik dan mengubah return type sebuah fungsi, yaitu diff yang dilihat reviewer, dan bukan mengisi field body yang sudah menunggu. Bentuk yang sama menjawab bagian tersulit section 44, yaitu incoming call yang wajib membangunkan telepon tetapi hanya boleh memuat call_id beserta penandanya: di sini itu adalah NotificationKind::IncomingCall dengan call_id di subject_id, dan SDP offer tidak muat di dalam Option<Id>. Section 77 meminta push token disimpan dalam bentuk hash, dan bila dibaca secara harfiah itu berarti token yang tidak dapat dikirimi apa pun, sehingga kredensialnya dipecah menjadi dua kolom dengan dua tugas berbeda: push_token menyimpannya dalam keadaan tersegel dengan key turunan dari deployment secret dan device id sebagai associated data sehingga dump tabel device bukan kumpulan kredensial push dan ciphertext yang dipindahkan ke baris device lain tidak akan terbuka, sedangkan push_token_hash adalah pegangannya, yaitu satu-satunya bentuk yang dipakai oleh setiap lookup, setiap deduplikasi, setiap baris log, dan setiap label metrik, dan justru itulah yang membuat aturan jangan pernah menulis token ke log dapat dipatuhi seseorang yang tetap harus mendiagnosis pengiriman. Kegagalan membuka token dijawab satu error yang sama untuk keempat penyebabnya, karena error yang membedakan base64 rusak dari authentication gagal adalah oracle bagi siapa pun yang dapat menulis ke kolom itu. RawToken ada untuk menandai batasnya, memiliki Debug tulisan tangan yang hanya mencetak panjang, dan di-drop di dalam register sehingga string mentahnya tidak pernah mencapai migo-store. Enam dari empat belas jenis notifikasi section 44 sengaja tidak menjadi baris, dan penentunya satu pertanyaan yaitu apakah menjawabnya membuat ia hilang dengan sendirinya: pesan yang belum dibaca menjawab ya karena conversation_cursor sudah memegang last_seq dan read_seq sehingga satu baris per pesan adalah penghitung yang sama disimpan di tempat kedua dan akan berselisih dengan yang pertama dalam sepekan, dan hal yang sama berlaku bagi voice note, mention, dan reply yang semuanya adalah pesan; permintaan pertemanan yang menggantung menjawab ya karena baris relationship itu sendiri adalah item inbox-nya; dan panggilan yang berdering menjawab ya dengan cara kedaluwarsa lalu menjadi missed call, yang adalah baris. Yang tersisa menjawab tidak, yaitu gift, level up, achievement, room invite, room announcement, event, game challenge, dan missed call, karena masing-masing meninggalkan jejak di suatu tempat tetapi tidak satu pun jejak itu mencatat apakah orangnya sudah melihatnya, dan daftar delapan itu hidup di notification_kind::is_storable yang dipanggil kedua backend sebelum insert sehingga aturannya ditegakkan dan bukan diingat. Empat hal menahan sebuah push dan tidak satu pun di antaranya adalah error, yaitu Connected ketika device masih memegang socket dan sudah menerima event-nya, Coalesced ketika device baru dibangunkan untuk kind yang sama beberapa saat lalu, Budget ketika bucket wake-up device itu habis, dan Stale ketika registrasinya lebih tua daripada yang dipercaya deployment; keempatnya dihitung, dikembalikan di dalam Delivery, dan dilaporkan Ok, karena gift yang gagal berbunyi tetap gift yang sampai sedangkan pemanggil yang menerima RATE_LIMITED dari sebuah gift tidak punya cara benar untuk menanggapinya. Connected adalah yang paling sering terjadi dan itu adalah sistem yang bekerja sesuai rancangan, sekaligus wujud kalimat section 44 bahwa push tidak dikirim untuk setiap event kecil. Coalescing memakai satu key per pasangan device dan kind lewat set_if_absent dan bukan baca-lalu-tulis, karena dua event yang tiba di dua node pada saat yang sama akan sama-sama membaca tidak ada tanda dan sama-sama mendorong push, sehingga check-then-set di sini adalah bug yang hanya muncul di bawah beban yang justru menjadi alasan keberadaannya, dan key-nya per device dan bukan per akun karena membungkam tablet dengan alasan telepon baru berbunyi bukan perhatian orang yang sama. Kind mendesak yaitu incoming call dan missed call melewati jendela coalescing tetapi tidak melewati budget, karena panggilan yang berdering hanya berguna beberapa detik sedangkan device yang dibanjiri percobaan panggilan tetap device yang dibanjiri. Kegagalan cache tidak pernah diteruskan sebagai error dan selalu gagal ke arah tetap membangunkan, sesuai section 173, karena push tambahan kepada orang yang sedang membaca aplikasinya hanyalah getar yang terbuang sedangkan push yang tertahan adalah pesan yang tidak pernah tiba. Crate ini tidak memutuskan siapa yang harus diberi tahu, karena room announcement sampai kepada anggota atas keputusan migo-rooms dan gift sampai kepada penerimanya atas keputusan migo-economy, sehingga di sini tidak ada pembacaan membership maupun social graph, dan satu hal yang tetap dicari yaitu device mana yang masih memegang socket ditanyakan kepada RoutingCache di layer 2 dan bukan kepada migo-presence di layer 3, karena dua crate layer 3 yang saling bergantung adalah cara sebuah dependency graph menjadi cycle. Metode register diletakkan pada trait store baru yaitu NotifyStore dan bukan sebagai tambahan pada DeviceStore, sebab dengan begitu migo-auth yang memegang DeviceStore untuk mendaftarkan device pada saat sign in secara struktural tidak memiliki metode apa pun yang dapat membaca atau menulis push token. Metode unregister tidak memungut budget sama sekali, karena client yang tidak dapat membatalkan registrasi adalah telepon yang terus berbunyi untuk akun yang sengaja ditinggalkan pemiliknya, dan anda melakukannya terlalu sering bukan jawaban yang dapat diterima atas berhenti memberi tahu saya. Tidak ada seri metrik berlabel device, berlabel push token, maupun berlabel hash token, karena hash adalah identitas per device yang stabil sehingga seri yang berkunci padanya adalah daftar kehadiran yang sama dengan satu langkah tambahan. Karena opcode 145 dan 146 masih SPEC, metode crate ini mengembalikan tipe domain yaitu Inbox, Delivery, dan u32, dan layer 4 yang memasangkan opcode-nya, mengikuti preseden migo-social. Tiga penyimpangan schema yang disengaja: tabel notification beserta dua index di atas kolom yang sama yang hanya berbeda predikatnya karena hitungan badge berjalan pada setiap aplikasi kembali ke depan sedangkan daftarnya hanya ketika seseorang menekan lonceng; empat kolom push pada tabel device yaitu push_token, push_token_hash, push_provider, dan push_updated_at, dengan push_updated_at sebagai satu-satunya cara sweeper berhenti membayar pengiriman kepada device yang sudah pergi; dan unique index push_token_hash, karena token menandai sebuah telepon dan bukan sebuah baris, sehingga backup yang dipulihkan ke handset baru membuat platform menyerahkan token yang sama kepada baris device yang kemarin belum ada, dan tanpa index itu baris lama tetap memegangnya sehingga satu notifikasi menjadi dua push ke satu telepon untuk selamanya. Satu penyimpangan protocol schema yang disengaja: tiga variant NotificationKind baru yaitu VoiceNote bernilai 12, MissedCall bernilai 13, dan IncomingCall bernilai 14, karena section 44 mendaftarkan empat belas jenis notifikasi sedangkan enum-nya baru memuat sebelas, dan tanpa ketiganya voice note akan dikirim sebagai Message sehingga aturan section 77 bahwa audio tidak boleh berada di dalam push menjadi aturan yang tidak dapat diperiksa siapa pun, sedangkan incoming call tidak akan memiliki kind sama sekali, dan 63 test yang menuntut bahwa kelima metode yang menghadap client menolak caller tanpa akun maupun tanpa device sebelum satu pun biaya dipungut, bahwa tidak satu pun byte yang diserahkan kepada transport memuat kalimat sedangkan token mentah hanya ada di sana dan tidak pernah di baris tersimpan, bahwa token tersimpan hanya terbuka dengan key turunan deployment secret dan hanya untuk device yang mendaftarkannya sehingga key deployment lain maupun device lain menerima satu error yang sama, bahwa token yang diterbitkan ulang oleh platform dimiliki pendaftar terakhir sehingga telepon yang sudah dijual tidak pernah berbunyi untuk akun pemilik sebelumnya, bahwa keempat alasan menahan push dihitung, dikembalikan di dalam Delivery, dan dilaporkan Ok, bahwa coalescing berkunci pada pasangan device dan kind sehingga tablet tidak dibungkam oleh telepon, bahwa kind mendesak melewati jendela coalescing tetapi tidak melewati budget, bahwa kegagalan cache selalu gagal ke arah tetap membangunkan, bahwa Unregistered adalah satu-satunya jawaban transport yang mengubah state tersimpan, bahwa baris notification hanya berisi id dan timestamp dengan destructure yang lengkap sehingga penambahan kolom teks berhenti dikompilasi, bahwa setiap platform diserahkan kepada transport bersama provider miliknya sendiri, dan bahwa tidak ada label device, label token, maupun label hash pada satu pun metrik. Suite ini menemukan dua cacat nyata yang keduanya sudah diperbaiki: kelima metode yang menghadap client tidak memeriksa identitas pemanggil sama sekali sehingga pemanggil tanpa identitas tetap dilayani; dan scope key coalescing ditulis memakai titik dua sedangkan CacheKey menolak apa pun selain huruf kecil ASCII, yaitu assertion yang membuat jalur coalescing panic pada setiap build debug sekaligus menolak underscore yang dipakai setiap scope lain di repository, sehingga assertion-nya dilonggarkan untuk underscore, titik dua tetap dilarang karena ia adalah pemisah key itu sendiri, dan scope milik crate ini diganti.
migo-games, yaitu trait Referee dengan 6 metode untuk catalogue, start, active, view, play, dan abandon, ditambah trait port Rewards dengan 2 metode yaitu award_experience dan mark_winner yang diimplementasikan composition root di atas Treasurer milik migo-economy sehingga crate ini tidak pernah menaut migo-economy, dengan bentuk inversi yang sama seperti Announcer pada migo-economy, PushSender pada migo-notify, Storage pada migo-media, dan Roster pada migo-moderation, dan Unrewarded adalah implementasinya bagi deployment maupun test yang memainkan dan memutus game persis sama hanya tanpa hadiah. Section 89 dan 90 menuntut server yang menjadi wasit, dan di sini itu berarti seluruh state sebuah game hidup sebagai byte string di kolom game_session.state yang opaque bagi store, client hanya mengirim Move, dan penentuan siapa pemain, giliran siapa, apakah sebuah langkah legal, dan siapa yang menang seluruhnya dihitung server di dalam engine, sehingga client tidak pernah memegang state dan karena itu tidak memiliki apa pun untuk dicurangi. Redaksi bukan proses membersihkan field melainkan ketiadaan field, karena tipe Render sama sekali tidak menyediakan tempat bagi angka rahasia guess-the-number maupun bagi tangan lawan yang belum terbuka pada rock-paper-scissors, sehingga fungsi render yang membaca seluruh kebenaran untuk menghitung umpan balik dan rentang yang tersisa secara struktural tidak dapat membocorkannya, dan tangan rock-paper-scissors baru terisi di Render ketika keduanya sudah masuk sehingga sama bagi setiap penonton. Replay dan lost update dikalahkan bukan oleh lock di service melainkan di storage layer lewat satu compare-and-set, yaitu advance_game yang hanya cocok bila updated_at masih sama dengan yang dihitung langkah itu dan status masih terbuka, sehingga play menjadi loop yang membaca ulang lalu menerapkan ulang hingga retry_budget habis: dua commit rock-paper-scissors yang benar-benar bersamaan sama-sama dihitung atas ronde yang sama, store meloloskan tepat satu dan menjawab None kepada yang kalah balapan, lalu yang kalah membaca ulang ronde yang kini separuh terisi dan berhasil menambahkan dirinya, sedangkan sebuah replay membaca ulang state segar tempat selnya sudah terisi atau tangannya sudah committed sehingga engine menolaknya sebagai langkah ilegal sebelum menyentuh store, dan client tidak pernah melihat maupun memasok token compare-and-set itu karena server memilihnya dari pembacaan segarnya sendiri. Satu-satunya sumber acak adalah OsRandom yang ditarik server pada saat create untuk angka rahasia guess-the-number, sehingga tidak ada seed yang pernah dilihat client dan tidak ada undian yang dapat diputar ulang. Seperti migo-economy crate ini tidak memiliki serde karena bukan ia yang berbicara di jaringan, sehingga state adalah byte string berlayout tetap yang diberi prefix satu byte versi dan dibangun dari Id::as_bytes, dan state yang tidak dapat didecode build ini adalah Corrupt yang menjadi internal error dan bukan penolakan yang dilihat client, sebab byte itu ditulis oleh crate ini sendiri. Berbeda dari migo-economy crate ini tidak memiliki cache sama sekali karena state game kecil, otoritatif, dan berubah pada setiap langkah, sehingga tidak ada pembacaan yang layak di-cache. Section 37 dan 87 melarang perjudian, maka default game tidak mempertaruhkan apa pun: hadiah hanya XP lewat award dengan Source::Game ditambah badge opsional lewat award_badge dengan Badge::GameChampion, tidak ada currency dan tidak ada pot, dan kolom stake_currency serta stake_amount sengaja dibiarkan kosong sebagai cadangan bagi perluasan teregulasi di kemudian hari. Tiga engine adalah tiga arketipe: tic-tac-toe yang turn-based dengan papan yang seluruhnya publik dan giliran ditegakkan dari paritas papan, rock-paper-scissors yang simultan dengan commit tersembunyi hingga keduanya masuk, dan guess-the-number yang single-player melawan angka rahasia server; engine() mengembalikan &'static dyn Engine karena setiap engine zero-sized dan stateless sehingga tidak ada state engine yang dapat menyimpang dari state di store. Otorisasi dibaca dari store lewat is_member dan bukan dipercaya dari pemanggil, sehingga bukan-anggota conversation dijawab NOT_FOUND yang menyembunyikan eksistensi game sesuai section 48, anggota yang bukan pemain boleh menonton tetapi langkahnya dijawab PERMISSION_DENIED, giliran yang salah dijawab CONFLICT, dan langkah yang tidak legal maupun yang tidak cocok dengan kind game dijawab VALIDATION_FAILED. abandon adalah NoContest tanpa hadiah bagi siapa pun, karena kemenangan-karena-lawan-menyerah adalah undangan bagi collusion farming, sehingga hanya finish alami lewat langkah penentu yang memanggil Rewards. Delta yang disiarkan section 39 adalah Started, Moved, TurnChanged, dan Finished yang masing-masing membawa game_id, dan Moved hanya menyatakan bahwa seorang pemain telah bergerak tanpa memuat langkahnya sehingga aman dikirim ke setiap pemain termasuk yang tidak boleh melihat isi langkah itu. Tidak ada seri metrik berlabel account maupun berlabel conversation, hanya GameKind, Rejection, dan Conclusion yang ketiganya enum tertutup, karena penghitung yang berkunci pada siapa bermain di mana adalah grafik sosial yang dijauhkan section 174 dari endpoint metrik. Berbeda dari migo-notify dan migo-economy yang opcode-nya masih SPEC, opcode game yaitu GAME_ACTION bernomor 176 dan GAME_EVENT bernomor 177 sudah SCHEMA dan handler keduanya kini sudah dipasang di layer 4, maka biaya tiap operasi client adalah konstanta lokal yang dipungut charge() mengikuti preseden migo-moderation dan migo-economy, metode crate ini mengembalikan tipe domain yaitu GameView, MoveResult, GameSummary, dan Vec<GameInfo>, dan layer 4 yang memetakan keduanya ke tipe wire, mengikuti preseden migo-social, migo-notify, dan migo-economy. GameView membawa field state_version yang tidak lain adalah updated_at milik store yang dirender sebagai angka opaque, yaitu token compare-and-set yang sama yang setiap langkah diterapkan terhadapnya, karena client yang menerima dua siaran di luar urutan tidak memiliki cara lain untuk mengetahui siaran mana yang menggambarkan papan yang lebih baru sedangkan menambah pencacah kedua di samping token itu akan menciptakan dua pengertian tentang state mana yang berlaku yang dapat saling bertentangan; ia opaque dengan sengaja, yaitu bukan hitungan langkah dan bukan waktu dinding yang layak ditampilkan client, dan satu-satunya operasi yang terdefinisi atasnya adalah perbandingan terhadap versi lain dari game yang sama, dan 95 test yang menutup ketiga game itu satu per satu beserta kerahasiaan yang membuat ketiganya berbeda: tangan yang sudah dikomit pada rock paper scissors tidak terlihat oleh lawan dan tidak dapat dibaca ulang bahkan oleh pemiliknya, render debug sebuah ronde yang setengah terkomit tidak memuat tangan siapa pun, komit kedua atas ronde yang sama ditolak dan justru itulah yang mematahkan replay, angka rahasia pada guess number tidak pernah muncul di bagian mana pun dari view yang diterima client bahkan setelah kesempatan menebak habis, dan penonton tidak belajar apa pun yang tidak dipelajari pemainnya; ditambah token CAS yang dibandingkan store adalah tepat state_version yang dilihat client sehingga tulisan terhadap token basi ditolak alih-alih diulang tanpa batas, dua komit yang dihitung terhadap ronde kosong yang sama masing-masing mendarat tepat sekali, ledger yang gagal tidak membatalkan game yang sudah selesai, dan deployment tanpa economy memainkan game yang sama persis
migo-bots, yaitu trait Bots dengan 7 metode untuk register, authenticate, rotate_token, set_scopes, set_paused, list, dan get, di atas trait store BotStore yang lahir bersama crate ini untuk menjangkau tabel bot yang sudah ada di schema. Sebuah bot adalah sebuah akun dan bukan jenis entitas baru, sehingga register menyerahkan tiga baris sekaligus yaitu account, profile, dan bot kepada BotStore::register_bot yang menuliskannya dalam satu transaksi, karena akun tanpa baris bot adalah akun yang tidak dapat dimasuki dan tidak ada yang tahu cara berbicara atasnya, dan karena itu tidak ada keadaan antara yang sah untuk ditinggalkan service ini bila terjadi crash. Setiap akun bot distempel hash Argon2id yang sah tetapi tidak dapat diverifikasi, dihitung satu kali di BotService::new dari 32 byte acak yang kemudian dibuang lalu di-clone untuk setiap bot, karena Argon2id berbiaya puluhan milidetik dan megabyte memori sehingga menjalankannya per registrasi akan menjadikan endpoint register sebuah tuas amplifikasi memori bagi penyerang yang men-scriptnya, sedangkan hash itu bukan rahasia karena tidak menjaga apa pun yang dapat dibuka sebuah password melainkan hanya nilai yang wajib ada dan wajib tidak pernah cocok, yaitu pilihan yang sama dengan hash akun-absen milik migo-auth. Token bot ditarik dari OsRandom, dikembalikan tepat satu kali pada register dan rotate_token, dan disimpan hanya sebagai tag HMAC-SHA-256 berkunci lewat MacKey::derive di bawah label yang terpisah dari kunci MAC lain, sesuai section 77 yang menuntut token disimpan dalam bentuk hash dan tidak pernah ditulis ke log, sehingga string mentahnya tidak pernah mencapai migo-store dan yang tersimpan hanyalah pegangan yang dipakai setiap lookup. Kegagalan authenticate menjawab satu error TOKEN_INVALID yang sama baik untuk token yang tidak dikenal maupun untuk bot yang telah dinonaktifkan, dan hanya metrik yang membedakan keduanya lewat AuthReject::Unknown dan AuthReject::Disabled sesuai section 161, karena error yang membedakan token yang belum pernah ada dari token yang dulu sah adalah oracle bagi siapa pun yang memegangnya. Setiap metode pengelolaan menyelesaikan bot lewat owned yang menjawab NOT_FOUND baik ketika bot tidak ada maupun ketika ia milik orang lain sesuai section 48, sehingga seorang pemilik tidak dapat menyelidiki bot id yang bukan miliknya. Scopes adalah bitmask atas keenam permission bot section 41 dengan default Scopes::NONE sesuai tuntutan permission minimum dan disimpan sebagai i64. Section 42 melarang bot memiliki akses database langsung, dan di sini itu berarti crate ini mengautentikasi serta mendeskripsikan bot tetapi tidak pernah menyerahkan handle store apa pun kepada bot itu sendiri. Token bot tidak kedaluwarsa sehingga authenticate tidak memiliki parameter waktu, dan jalur pencabutannya adalah rotate_token yang mengganti tag lama atau set_paused yang mengeset bot.disabled_at, dan keduanya membuat token berikutnya jatuh ke TOKEN_INVALID yang sama. Penonaktifan oleh moderator yang dicatat di migo-moderation sebagai DisableBot mendarat pada kolom bot.disabled_at yang sama lewat BotStore, yaitu kolom yang tidak terjangkau trait mana pun sebelum crate ini lahir. Cap jumlah bot per pemilik ditegakkan lewat bots_for_owner dan dijawab QUOTA_EXCEEDED, tabrakan username diterjemahkan dari ALREADY_EXISTS milik store menjadi USERNAME_TAKEN, dan webhook divalidasi https-saja dengan batas panjang sehingga nilai kosong berarti tanpa webhook dan bukan sebuah error. Karena opcode bot 178 sampai 180 masih SPEC, biaya tiap operasi client adalah konstanta lokal yang dipungut charge() mengikuti preseden migo-moderation dan migo-economy, dan metode crate ini mengembalikan tipe domain yaitu Registered, BotView, BotIdentity, dan Secret sementara layer 4 yang memasangkan opcode-nya, mengikuti preseden migo-social, migo-notify, migo-economy, dan migo-games. Tidak ada seri metrik berlabel account, bot, maupun owner, hanya AuthReject yang enum tertutup dan penghitung tak berlabel, karena penghitung yang berkunci pada pemilik atau bot adalah daftar kepemilikan yang dijauhkan section 174 dari endpoint metrik, dan 96 test yang menutup kerahasiaan token yaitu bentuk Debug yang meredaksi, tag 32 byte yang disimpan alih-alih token itu sendiri, dan kegagalan authenticate yang byte per byte identik antara token yang tidak dikenal, token yang salah bentuk, token yang sudah dicabut, dan bot yang dinonaktifkan; kepemilikan yaitu bot milik owner lain yang dijawab NOT_FOUND dan bukan PERMISSION_DENIED sesuai section 48 pada setiap metode manajemen; otoritas yang selalu dibaca dari baris dan bukan dari view yang basi dengan bit tinggi yang tidak terdefinisi dijatuhkan alih-alih dihormati; harga setiap metode yang dipungut dari budget akun sebelum pencarian kepemilikan; dan pemanggil tanpa identitas yang ditolak sebelum limiter disentuh sehingga permintaan yang menyebut akun orang lain tidak dapat menguras budget akun itu
migo-federation, yaitu trait Mesh dengan 17 metode untuk add_peer, set_peer_status, peers, peer, region, hello, prove, authenticate, check_sequence, reset_link, check_epoch, epoch, bump_epoch, enqueue, due, mark_delivered, dan mark_failed, dibangun di atas handshake node milik migo-crypto sehingga crate ini tidak menuliskan satu pun primitif kripto sendiri melainkan hanya menyusun store, handshake, dua pertahanan replay, dan metrik menjadi satu subsistem, dengan bentuk service yang sama seperti saudara-saudaranya. Batas mesh adalah allow-list dan bukan siapa pun yang berhasil menyambung, sesuai section 170 yang menuntut join eksplisit dan disetujui operator tanpa penemuan otomatis: add_peer adalah satu-satunya jalan sebuah node dikenal, dan setiap handshake dimulai dengan mencari peer di dalamnya sehingga node yang tidak pernah didaftarkan ditolak sebelum payload-nya didecode, yaitu perbedaan antara sebuah mesh dan sebuah endpoint publik. Node yang tidak dikenal, yang dijeda, yang diblokir, tanda tangan yang salah, jam yang menyimpang, dan nonce yang diputar ulang semuanya dijawab satu mesh_auth_failed yang sama tanpa apa pun di detail publiknya, karena selisih antara aku tidak mengenalmu dan tanda tanganmu salah adalah oracle bagi peer yang menyelidik, sesuai aturan error-sama section 48, section 161, dan handshake yang gagal-tertutup section 169; hanya metrik yang membedakan alasannya lewat enum tertutup HandshakeReject dan ReplayReason sehingga operator yang mengamati lonjakan blocked versus proof_invalid sedang mendiagnosis dua serangan berbeda sementara peer hanya tahu ia ditolak. Dua pertahanan duduk di bawah handshake dan keduanya stateful milik crate ini: jendela nonce mengingat nonce acak 32 byte setiap handshake terkini dan menolak pengulangan sehingga hello yang ditangkap lalu diputar ulang tidak dapat mengautentikasi lagi di dalam jendela toleransi yang dituntut section 169, dan urutan per-link menuntut setiap paket membawa nomor tepat satu lebih besar dari yang terakhir sehingga nomor yang tidak maju adalah replay yang di-drop sedangkan gap adalah replay yang dicurigai atau segmen yang hilang yang menurut section 152 wajib mereset link dan bukan diterima diam-diam, dan keduanya hanya menjaga amplop dan tidak pernah membaca payload karena amplop adalah satu-satunya yang boleh dilihat node perantara. Panjang jendela nonce wajib melebihi dua kali skew jam yang diterima karena proof diterima dalam rentang plus minus skew sehingga jendela tempat sebuah replay masih dapat lolos pemeriksaan jam selebar dua kali skew, dan memori nonce harus hidup lebih lama darinya atau replay menyelinap di celah antara kedua pertahanan itu, dan validasi ini dilakukan sekali pada saat konstruksi dan bukan per request. Event yang menuju region lain tidak dikirim inline melainkan ditulis ke outbox FederationStore dalam transaksi yang sama dengan perubahan yang diumumkannya lalu ditarik sender sesudahnya, dan itulah yang membuat pengiriman selamat dari restart sekaligus menjadikannya at-least-once sehingga setiap konsumen federation wajib idempotent sesuai section 153, sedangkan mark_failed mendorong percobaan berikutnya keluar pada backoff eksponensial yaitu base kali dua pangkat attempts yang diclamp ke cap sehingga region yang mati berbiaya trickle yang meluruh dan bukan hot loop. Crate ini tidak membuka socket dan tidak membingkai byte, karena metode handshake-nya menerima dan mengembalikan pesan migo-crypto sedangkan transport yang membawanya milik gateway, dan ia tidak memegang tabel routing maupun peta shard melainkan hanya sebuah epoch routing yaitu penghitung monotonik yang dinaikkan composition root sehingga ia dapat menjawab routing_epoch_stale bagi request yang dibuat atas pandangan mesh yang telah usang. Frame federation adalah MWP biner dan bukan JSON sesuai section 169, dan opcode-nya wajib jatuh di pita federation yang dicadangkan yaitu 208 sampai 223, yang memuat opcode 208 sampai 221 yang ditetapkan section 169 beserta cadangan di atasnya hingga batas pita call di 224, sehingga sebuah frame mesh tidak pernah tertukar dengan frame client dan enqueue menolak opcode di luar pita itu. Node secret tiba lewat open dan tidak pernah dibaca dari disk oleh crate ini maupun ditransmisikan, karena ia hanya menandatangani proof, dan randomness disuntikkan dengan cara yang sama sehingga sebuah simulasi dapat menggerakkan seluruh mesh secara deterministik. PeerStatus dipetakan ke i16 di store dengan nilai 0, 1, dan 2, dan nilai yang tidak dikenal didecode ke Blocked yaitu arah yang aman sehingga satu baris yang korup atau berniat jahat menutup satu link dan bukan menggagalkan setiap handshake yang membaca tabel, yaitu asimetri yang sama seperti hash akun-absen pada migo-auth dan migo-bots. Tidak ada seri metrik berlabel node maupun peer, hanya HandshakeReject dan ReplayReason yang keduanya enum tertutup beserta penghitung tak berlabel, karena penghitung yang berkunci pada peer adalah daftar anggota mesh yang dijauhkan section 174 dari endpoint metrik, dan 71 test yang menutup handshake yaitu proof yang dipisahkan domainnya, kunci yang salah, nonce yang diulang di dalam jendela clock, dan timestamp di luar jendela skew yang keempatnya ditolak dengan satu error opaque yang sama sementara metrik tetap dapat membedakan keempat alasannya; urutan link yaitu paket pertama yang wajib bernomor satu, sequence nol yang tidak pernah diterima, sequence yang tidak maju sebagai replay, dan gap yang mereset link; epoch routing yang monotonik dengan epoch yang lebih tua dinyatakan stale; antrean keluar yaitu backoff yang berlipat dua sampai cap, event yang berulang kali gagal yang tidak pernah dibuang diam-diam, ack yang idempotent, dan event yang sudah terkirim yang tidak dapat dibangkitkan lagi oleh kegagalan yang datang terlambat; ditambah render metrik yang tidak memuat satu pun identifier, url, domain, maupun payload
migo-gateway, yaitu transport realtime itu sendiri: mesin state siklus hidup koneksi section 149, handshake dan resume section 138 sampai 140 dan 150, antrean backpressure tiga kelas section 151, dan hub langganan yang menyiarkan satu event ter-encode ke banyak socket section 136. Crate ini tidak tahu arti satu pun opcode aplikasi dan tidak menyebut satu pun crate domain, karena segala sesuatu di atas transport menjangkaunya lewat dua trait yang keduanya diimplementasikan composition root: Transport yang mengadaptasi socket konkret turun menjadi verba byte recv, send, dan close, dan Dispatcher yang mengadaptasi opcode aplikasi naik ke crate domain yang tidak boleh dijangkau gateway, dengan bentuk inversi yang sama seperti port pada saudara-saudara layer 3-nya. Dispatcher menjawab dua pertanyaan, bukan satu: selain "dispatch" untuk opcode aplikasi, ada "authorize_topics" yang ditanya gateway setiap kali SUBSCRIBE tiba, dan keputusan itu hanya dibaca dari dispatcher lalu dipakai untuk memilah granted dari rejected sebelum hub memegang apa pun, sehingga otorisasi topik Conversation, Room, dan User tidak pernah jatuh ke tangan transport yang tidak bisa menjawab apa itu. Gateway hanya memanggil dispatcher untuk session yang sudah Ready, sesudah handshake, autentikasi, gerbang fase, dan pemeriksaan rate semuanya lolos, sehingga tidak ada satu jalur pun tempat opcode aplikasi menyentuh domain sebelum keempatnya selesai. Sebuah session hidup dalam dua fase sesudah handshake yaitu AwaitingAuth dan Ready dengan satu gerbang: opcode ber-AuthLevel None yaitu Hello, Ping, Ack, dan Authenticate boleh di fase mana pun sedangkan setiap opcode lain menuntut Ready sesuai section 149, dan interval sebelum HELLO bukan sebuah fase melainkan milik langkah handshake sebelum sebuah session dan karenanya sebuah fase ada. Hub menyiarkan dengan encode sekali lalu menyerahkan Bytes clone yaitu satu bump reference count dan bukan salinan sesuai aturan hot-path section 136, dan tidak pernah memegang lock shard saat mendorong ke mailbox sehingga satu client lambat tidak menghentikan fan-out, sedangkan langit-langit langganan per session ditegakkan di sini supaya client tidak dapat memaku memori server tak terbatas dengan berlangganan ke segala hal. Backpressure section 151 menegakkan tiga nasib dari satu DeliveryClass: Critical tidak pernah di-drop melainkan menandai session lagging lalu menutupnya bila antrean penuh melewati deadline sehingga client melakukan resume, Coalescable menciut karena nilai baru untuk key yang sama menimpa yang lama di tempatnya, dan Droppable di-drop diam-diam tetapi selalu dihitung karena frame yang lenyap tanpa metrik adalah bug yang tidak ditemukan siapa pun berbulan-bulan. Hanya frame Critical yang membawa frame_seq dan hanya frame Critical yang disimpan di ring resume section 150, dan satu ACK kumulatif menggeser watermark yang memangkas ring sehingga satu ACK menuntaskan ratusan frame, sedangkan ring itu sekaligus buffer pengiriman ulang karena frame Critical yang belum di-ACK adalah tepat frame yang masih ada di ring. Admission control menegakkan langit-langit session section 149 dengan menaikkan penghitung lebih dulu lalu mengembalikannya saat overflow sehingga dua handshake yang berlomba tidak pernah keduanya lolos melewati langit-langit satu, dan store resume dibatasi langit-langit yang sama serta disapu dari buffer kedaluwarsa sebelum menyisipkan yang baru sehingga ledakan session yang putus tidak menumbuhkannya tanpa batas. NoopDispatcher adalah dispatcher tanpa logika aplikasi yang menjawab setiap opcode aplikasi dengan FEATURE_DISABLED, sehingga node dapat berdiri dan berbicara protokol transport penuh tanpa satu pun crate domain terpasang, berguna untuk test tingkat transport dan untuk node yang sengaja hanya melayani permukaan handshake, dan jawaban default "authorize_topics" adalah menolak segalanya sehingga node tanpa domain pun tidak pernah secara tidak sengaja memberi subscription. Satu hal yang sengaja tidak ada di sini: route /ws, adapter dari WebSocket ke Transport, dan seluruh penyebutan axum tinggal di migod pada layer 5 dan bukan di crate ini, karena migo-api pada layer 4 tidak boleh bergantung pada gateway yang juga layer 4, dan sebuah crate transport yang menyebut axum akan menyeret aturan pengikatan section 138 yaitu satu frame MWP per pesan biner, deflate mati, dan langit-langit ukuran frame yang keras ke seluruh driver alih-alih menahannya di satu adapter, dan 21 test yang menutup tujuh dari delapan invariant yang ditulis di kepala suite-nya, yaitu urutan yang ditegakkan berupa frame pertama yang wajib HELLO, HELLO kedua yang menutup koneksi, body HELLO yang tidak dapat di-decode yang tetap dijawab di bawah opcode HELLO, versi protokol yang tidak didukung, token inline yang buruk yang tidak fatal melainkan membuka session tanpa autentikasi, dan penolakan yang hanya mengungkap wajah publiknya; penutupan atas kemauan server berupa shutdown yang menyerahkan RECONNECT_HINT berjitter pada correlation nol sebelum menutup dan dua heartbeat yang terlewat yang menutup session bisu lalu mengembalikan slot-nya, keduanya berjalan di atas clock virtual sehingga timer produksi tidak perlu ditunggu; otorisasi yang dibaca dari dispatcher dan bukan dipercaya dari frame, dalam tiga bentuk: dispatcher tanpa domain yang menolak setiap topik, dispatcher yang hanya mengabulkan topik akun pemanggil sendiri, dan dispatcher yang mengabulkan segalanya dan diuji terhadap langit-langit langganan; dan limit yang berlaku tepat di batasnya, berupa frame SUBSCRIBE yang melebihi langit-langit langganan per session yang menolak surplus sebelum domain ditanya. Empat invariant lain di kepala suite itu yaitu backpressure yang terbatas dan gagal menutup, pemeriksaan ukuran sebelum parse, wire yang push-only, serta higiene log dan metrik kini tertutup pada commit ini: backpressure dengan tiga kelas delivery yang satu antrian terisi penuh, parse-before-alloc dengan frame oversize yang ditolak sebelum "Frame::decode" menyentuh header, wire push-only dengan pembuktian struktural terhadap enum opcode ditambah pembuktian perilaku terhadap opcode server-to-client yang dikirim client, dan hygiene dengan empat jalur kesalahan yang diperiksa untuk kebocoran penanda internal ke klien dan untuk metrik registry yang tidak memuat id akun atau perangkat.
migo-api, yaitu permukaan REST/JSON pada layer 4 yang diizinkan section 118: bukan cermin dari transport melainkan pelengkapnya, karena satu client tidak dapat membuka socket realtime tanpa access token dan tidak dapat memperoleh access token lewat socket yang belum ia buka. Karena itu crate ini boleh menyebut axum secara langsung, tidak seperti crate transport-agnostic pada section 138, dan mengekspos satu fungsi router yang membangun pohon route, state-nya, dan middleware section 119 serta 121 lalu mengembalikan satu axum Router yang dipasang migod. Hanya permukaan non-realtime yang tinggal di sini: bootstrap autentikasi yaitu register, login, refresh, dan logout, lalu probe health dan ready, scrape metrics, dan dokumen config yang dibaca satu kali saat startup, ditambah data plane media section 168 yaitu satu PUT dan satu GET di bawah /media/{key} yang hanya dipasang ketika backend storage adalah filesystem, karena URL yang dimintanya menunjuk proses ini sendiri; backend S3 melayani byte-nya sendiri dan proses ini menjawab 404 alih-alih berpura-pura menjadi object store, sementara PUT menegakkan langit-langit media.max_upload_bytes, GET menyajikan content type dari magic byte lewat sniff dan menolak byte yang scanner tolak, keduanya menolak key yang mencoba keluar dari root media, dan port MediaFiles didefinisikan di crate ini dan diimplementasikan migod atas FsStorage sehingga migo-media tetap tidak pernah melihat HTTP. Route media sengaja tidak berada di bawah /v1: byte di balik satu key tidak berubah ketika API berganti versi. Setiap pengiriman atau penerimaan chat, presence, typing, reaction, dan seluruh event realtime lain tetap dilarang berbentuk JSON dan hidup di socket lewat gateway, sehingga tidak ada satu jalur pun tempat pesan aplikasi mengalir sebagai JSON REST. Setiap handler menapaki pipeline section 119 dalam urutan yang sama: sebuah charge rate-limit tepi berbasis IP pada tiga endpoint bootstrap yang belum terautentikasi memakai trust tier paling ketat sebagai pertahanan berlapis, lalu panggilan domain yang mengautentikasi, memvalidasi, mengeksekusi, dan mengaudit. Error dikumpulkan lewat satu corong ApiError yang membungkus Error dari registry, memetakan kode ke status HTTP lewat fault http_status yang di-generate dari errors.json, dan hanya menuliskan public_message ke wire sesuai section 161, dengan header Retry-After yang diisi dari retry_after bila ada. Autentikasi diekstrak sebagai Authenticated yang lebih dulu memverifikasi access token tanpa I/O untuk membaca device id dari klaimnya lalu memanggil authenticate untuk memperoleh Identity yang status pencabutan tokennya sudah diperiksa, sedangkan RequestFacts memungut ip, user agent, dan request id dari header dengan hop pertama X-Forwarded-For, dan IdempotencyKey membaca kunci idempotensi yang wajib diterima setiap operasi pengubah keadaan section 118. Semua listing memakai cursor pagination lewat Page dan PageParams dengan langit-langit ukuran halaman yang ditegakkan dan di-clamp di sisi server, bukan dipercayakan kepada client. Middleware section 119 dan 121 merangkai TraceLayer, RequestBodyLimitLayer yang menegakkan langit-langit ukuran body, CORS yang dibangun dari config, dan propagasi request id. Access dan refresh token memang menyeberang di GrantResponse karena keduanya kredensial milik pemanggil sendiri yang baru saja membuktikan identitasnya, tetapi tidak pernah boleh menyentuh log section 145. Satu hal yang sengaja tidak ada di sini: route /ws, adapter dari WebSocket ke Transport, dan konstruksi service domain semuanya tinggal di migod pada layer 5, karena migo-api tidak boleh bergantung pada migo-gateway yang juga layer 4, dan crate ini hanya memegang handle service yang sudah jadi alih-alih membangunnya, dan 69 test yang memperlakukan permukaan HTTP-nya sebagai permukaan yang menghadap publik: password yang salah dan akun yang tidak dikenal yang byte per byte identik, fault internal yang tidak mengungkap apa pun baik di body maupun di header, tidak ada route admin, debug, reset, maupun proxy file, CORS yang tidak pernah menjawab wildcard, body yang bukan JSON dan content type yang salah yang menjadi client error dan bukan fault, body yang kelewat besar yang ditolak sebelum handler, limit paging yang selalu di-clamp dan tidak pernah ditolak, rate limit registrasi yang berkunci pada jaringan pemanggil sehingga belanja satu jaringan tidak menyentuh budget jaringan lain, dan render metrik beserta dokumen config yang tidak memuat identifier, token, maupun IP pemanggil yang utuh
migod, yaitu composition root pada layer 5 dan satu-satunya crate yang boleh menyebut setiap crate lain, sebuah binary tipis di atas sebuah library sehingga App::build menyusun seluruh sistem sementara main hanya memasang subscriber log lalu menyerahkannya untuk melayani, dan sebuah harness integrasi dapat membangun App yang sama di atas backend in-memory lalu menggerakkan satu service secara langsung tanpa pernah membuka socket, yang justru alasan setiap field App bersifat publik. App::build berjalan ketat dari bawah ke atas karena setiap layer adalah argumen bagi layer di atasnya: config divalidasi lebih dulu sehingga bind yang salah atau bucket rate limit yang mustahil gagal sebelum satu koneksi database pun dibuka, lalu satu Registry metrik dan satu Shutdown, lalu platform layer 2 yaitu store, cache, dan ratelimit, lalu keempat belas service domain layer 3, dan terakhir kedua transport layer 4, dengan Registry yang dioper lewat reference selama service dibangun lalu dibungkus Arc bagi API sesudah pinjaman terakhirnya sehingga yang dirender endpoint metrics adalah instance yang sama dan bukan yang kedua, dan satu Shutdown yang sama menjadi tempat gateway menguras session-nya sekaligus tempat server axum berhenti menerima koneksi sehingga satu SIGTERM menggerakkan seluruh proses ke arah berhenti yang bersih. Inversi port yang dijanjikan setiap crate layer 3 ditutup di sini dan hanya di sini: FsStorage mengimplementasikan Storage milik migo-media di atas filesystem sebagai pengganti object store untuk pengembangan sesuai section 168, StaffRoster mengimplementasikan Roster milik migo-moderation dari konfigurasi dan berpostur kosong sehingga tidak seorang pun berstatus staff dan setiap aksi operator ditolak sampai sebuah roster nyata dipasang, EconomyRewards mengimplementasikan Rewards milik migo-games di atas Treasurer milik migo-economy sehingga game memberi XP dan badge tanpa migo-games menaut migo-economy, sedangkan NoPush dan Silent adalah implementasi PushSender dan Announcer bagi build tanpa layanan push maupun pengumuman, dan Catalogue diisi gift default; kelima adapter itu adalah satu-satunya tempat dua saudara layer 3 bertemu, bukan panah langsung di antara keduanya. Kedua transport dipasang berdampingan pada satu axum Router yang berbagi satu socket dan tidak berbagi apa pun selain itu, tepat seperti dituntut kedudukan keduanya sebagai saudara layer 4: router REST milik migo-api membawa route dan middleware-nya sendiri, dan satu route WebSocket pada /ws meng-upgrade koneksi, memungut user agent dan ip yang kebetulan diketahui transport lalu menyusun RequestContext per koneksi, dan menyerahkan socket kepada gateway untuk seumur hidupnya. WsTransport adalah adapter dari axum WebSocket ke trait Transport milik gateway dan satu-satunya tempat di server yang menyentuh framing WebSocket, binary-only sesuai section 138 sehingga pesan teks adalah pelanggaran yang ditolak dan bukan didecode, dan ping serta pong dilewati karena keduanya keepalive milik layer WebSocket dan bukan frame aplikasi; socket-nya dibungkus Mutex bukan untuk mengunci apa pun melainkan karena axum WebSocket bersifat Send tetapi bukan Sync sedangkan gateway meminjam koneksi secara shared melewati sebuah await pada beberapa langkah handshake-nya sehingga koneksi hanya Send bila transport-nya Sync, dan Mutex membuatnya Sync tanpa biaya runtime karena setiap metode Transport memegang &mut self dan menjangkau socket lewat get_mut yang tidak pernah mengambil lock. AppDispatcher adalah satu-satunya implementasi Dispatcher milik gateway, menautkan opcode aplikasi ke crate domain dengan empat langkah yang selalu berurutan sama yaitu membangun Caller domain dari Identity yang telah dibuktikan gateway, mendecode body terhadap tipe yang dinamai opcode, memanggil tepat satu metode service, lalu menjawab atau melakukan fanout mengikuti return type dan bukan sebuah tabel, dengan setiap fanout mengecualikan device asal sesuai section 156. Seluruh 21 opcode client-to-server pada IDL kini terutekan, yaitu 15 di AppDispatcher untuk messaging, presence, rooms, keys, social, dan games ditambah 6 yang diselesaikan gateway sendiri sebagai bagian dari handshake dan langganan, sehingga tidak satu pun opcode yang benar-benar dikirim client lagi dijawab FEATURE_DISABLED; arm penutup yang menjawab FEATURE_DISABLED sambil menamai opcode-nya tetap ada mengikuti preseden NoopDispatcher milik gateway, bukan sebagai lubang yang masih terbuka melainkan supaya opcode yang ditambahkan ke IDL di kemudian hari dijawab dengan sebuah nama alih-alih menggagalkan kompilasi crate ini di tempat yang tidak berhubungan dengan penambahan itu. Satu pengecualian yang disengaja terhadap aturan rumah section 156 ada di GAME_EVENT: fanout-nya memakai publish dan bukan publish_excluding_self, karena aturan itu berlaku selama balasan permintaannya sendiri sudah membawa hasilnya sedangkan balasan GAME_ACTION menurut IDL adalah Acknowledged yang tidak membawa apa pun, sehingga pemain yang dikecualikan dari fanout langkahnya sendiri tidak akan pernah mengetahui giliran siapa berikutnya maupun bahwa game telah berakhir; delta section 39 aman bagi setiap pemain secara konstruksi sebab Moved hanya menyatakan bahwa seseorang telah bergerak dan tidak memuat langkahnya. Scope yang dipasok client tidak pernah dipercaya: room_id dan action_id pada GameAction tiba lalu dibuang, topic fanout diambil dari conversation_id milik GameView sehingga tidak ada client yang dapat menyiarkan ke conversation yang bukan miliknya, dan replay dikalahkan compare-and-set di store dan bukan oleh id yang dipilih client. Field UserProfile yang tidak dapat dijawab jujur dibiarkan kosong dan bukan didefault: public_id diturunkan lewat Id::public_id dan tidak pernah disimpan, avatar_url tetap kosong karena section 168 melarang server memproksikan byte media sedangkan mencetak signed URL di sini akan menaruh kredensial berkedaluwarsa di dalam response yang dapat di-cache, dan level, presence, badges, verified, serta custom_status kosong karena verified bernilai false yang didefault pada akun yang benar-benar terverifikasi adalah jawaban yang salah yang mengenakan bentuk sebuah jawaban. Satu root secret menurunkan material tanda tangan bagi ticket media, push token, dan bot token, diambil dari node.signing_key dan tidak pernah dari sumber lain: deployment production tanpa key menolak start sesuai section 103 alih-alih mencetak token di bawah default yang tidak dapat dirotasi siapa pun, sedangkan development dan staging memakai 32 byte acak yang efemeral disertai peringatan bahwa token tidak akan selamat dari restart, dan secret itu tidak pernah menyentuh log sesuai section 145, dan 63 test yang menutup ketiga hal yang hanya dapat salah di sini: parsing argumen yaitu --help dan --version yang tidak menyentuh config, logging, maupun socket sementara argumen yang tidak dikenal tetap ditolak walaupun mengikuti flag yang dikenal, dengan exit code 2 sesuai konvensi; penolakan startup yaitu production dan staging yang menolak token key yang kosong, terlalu pendek, atau sama dengan konstanta development dan menolak kredensial database yang terdokumentasi, semuanya tanpa menggemakan nilai yang ditolak, ditambah bentuk Debug config dan ringkasan yang aman ditulis ke log; dan composition root itu sendiri yaitu graph yang terbangun utuh, dua kali build yang menghasilkan instance yang independen, registry metrik yang tidak membawa identitas node, jembatan economy yang meneruskan credit sebuah game secara idempotent per game dan menolak akun yang tidak ada alih-alih menelannya, roster staff yang tidak memberi apa pun kepada orang yang tidak terdaftar, dan storage filesystem yang menolak traversal, key kosong, maupun key absolut
packages/protocol, yaitu paket TypeScript dari hasil generate yang sama dengan sisi Rust, dengan tsconfig composite dan 11 test yang memeriksa bahwa setiap opcode, error code, feature bit, dan flag di IDL benar-benar muncul di kode hasil generate
packages/wire, yaitu codec frame TypeScript untuk varint, zigzag, MSE, flag, limit, id base32, timestamp epoch Migo, dan kompresi DEFLATE mentah lewat CompressionStream bawaan platform dengan batas inflasi yang diperiksa di dalam loop baca bukan sesudahnya, dan 16 test yang dijalankan terhadap file vector yang sama dengan sisi Rust
packages/crypto, yaitu HKDF label, XChaCha20-Poly1305, dan token HMAC di atas @noble/hashes dan @noble/ciphers tanpa satu pun primitive yang ditulis sendiri, dengan 21 test yang dijalankan terhadap file vector yang sama dengan sisi Rust ditambah case yang hanya bisa terjadi di JavaScript, yaitu byte key tidak boleh muncul di hasil String, JSON.stringify, maupun util.inspect, key yang dibungkus WAJIB menyalin byte-nya sehingga buffer pemanggil tidak bisa mengubah key setelahnya, dan key yang sudah dihapus WAJIB menolak dipakai alih-alih menghasilkan tag yang terlihat masuk akal dari key nol
packages/sdk, yaitu MigoClient di atas rantai packages/wire, packages/protocol, dan packages/crypto yang ketiganya sudah BUILT, dengan register, sign in, resume, sesi gateway, distribusi sender key, percakapan direct dan group, pengiriman pesan tersegel, presence, dan abstraksi KeyStore bagi material kunci yang default-nya in-memory sehingga tidak ada private key yang meninggalkan proses, seluruhnya lulus tsc --build dalam mode strict tetapi belum memiliki satu pun test. Di sinilah cryptographic envelope section 11 pertama kali dikodekan, yaitu di src/session-crypto.ts, dan clients/desktop maupun clients/android menyalin layout-nya field demi field dari sana, sehingga ketiganya bukan tiga spesifikasi melainkan satu layout dengan tiga penulis: envelope_version, scheme, sender_key_id, lalu preamble X3DH yang hanya ada pada scheme prekey, lalu ratchet_public_key, message_counter, previous_chain_length, dan ciphertext sampai akhir yang enam belas byte terakhirnya adalah tag AEAD. Bahwa scheme dan bukan sebuah flag yang menentukan field mana yang hadir adalah keputusan yang disengaja, sebab boolean yang nilainya diam-diam menambahkan seratus byte ke layout adalah cara parser berakhir tidak sepakat tentang di mana ciphertext dimulai, dan 56 test yang menutup provenance kripto yaitu setiap primitif yang datang dari tiga library @noble yang diaudit dan bukan dari implementasi sendiri sementara layer SDK tidak mengimpor kriptografi apa pun miliknya sendiri, kerahasiaan kunci yaitu satu sesi 1:1 penuh maupun penanganan sender key yang sama sekali tidak menyentuh web store sehingga tidak ada private key yang pernah ditulis ke localStorage sesuai section 178, padding yang membulatkan plaintext ke bucket tetap sehingga dua pesan pendek yang berbeda isi tersegel dengan panjang yang sama, transport yang menaruh socket-nya ke mode biner sebelum handshake dan tidak pernah mengirim satu pun string teks maupun JSON serta tidak pernah menarik data realtime di atas timer, dan error yang absen hint-nya tetap menyisakan symbol-nya berdiri sendiri dengan kegagalan autentikasi yang tidak dapat dibedakan antara akun yang tidak ada dan password yang salah
clients/web, yaitu PWA Next.js di atas packages/sdk yang lulus tsc --noEmit dalam mode strict, dengan seluruh data realtime mengalir sebagai frame biner di atas WebSocket dan bukan JSON, dan snapshot KeyStore disimpan di IndexedDB dan bukan di localStorage, sessionStorage, maupun cookie sesuai section 178, tetapi belum memiliki test. Ia sepenuhnya client side sesuai output export Next sehingga artefaknya adalah satu direktori berkas statis yang dapat dilayani host statis mana pun, CDN, maupun tools/serve.mjs di paket itu sendiri dengan byte yang identik, dan tidak ada proses server yang perlu berdiri di antara pengguna dan kuncinya sendiri sebab tidak ada apa pun untuk dirender di sisi server ketika server tidak dapat membaca apa pun; karena tidak ada server, route dinamis /chat/[id] tidak dapat diprerender sehingga conversation yang terbuka hidup di fragment URL yang tidak pernah diterima host statis, dan skrip dev-nya berjalan di port 19991, dan 63 test yang menutup apa yang boleh dan tidak boleh dilihat pengguna: tidak satu pun private key atau token yang ditulis ke localStorage, sessionStorage, maupun cookie sedangkan snapshot key store dan grant session bolak-balik lewat IndexedDB di bawah key yang terdokumentasi, tidak satu pun string yang dikendalikan server yang dirender sebagai elemen HTML hidup sehingga caption dan isi pesan yang bermusuhan tampil sebagai teks yang di-escape dan inert, error yang tidak terduga yang tidak pernah membocorkan message, stack, path, maupun cause-nya sementara symbol mesin tetap tersembunyi sehingga lookup yang dibatasi privasi tidak dapat dibedakan dari yang tidak ada sesuai section 180, presence yang tidak pernah menyingkap user yang Invisible, dan href percakapan yang membawa id di fragment dan bukan di path maupun query sehingga id yang memuat metakarakter URL tidak dapat menyelundupkan parameter fragment kedua
migo-captcha, yaitu tantangan captcha gambar untuk permukaan bootstrap publik,
standar di seluruh aplikasi sejak keputusan itu dibalik. Versi pertamanya adalah
kode numerik enam-digit yang dibawa teks di respons, dengan alasan bahwa sinyal
perilaku cukup untuk gerbang teman-atau-bot; postur itu menua buruk karena kode
teks di body respons selesai dipecahkan oleh script yang sama yang membaca body,
sehingga gerbangnya hanya memperlambat satu request. Sekarang setiap tantangan
adalah PNG yang dirender server-side: lima sampai enam karakter alfanumerik
huruf besar dari alfabet tanpa karakter ambigu (I, O, S, 0, 1, 5 dikeluarkan),
digambar dengan tiga font TTF yang di-embed include_bytes (Liberation Sans,
Serif, dan Sans Narrow Bold) supaya kesulitan tantangan identik di semua mesin
build dan run, dengan rotasi, skala, jitter baseline, dan spacing per karakter
yang diacak, latar ber-dot dan ber-speckle ber-opacity rendah, wobble per-baris
yang membengkokkan goresan vertikal, dan tepat satu kurva interferensi
Catmull-Rom yang knot-nya ditempatkan struktural di dalam pita tinta setiap
karakter plus jangkar di luar kanvas kedua sisinya, sehingga kurva dijamin
melintasi semua karakter dari sisi ke sisi dan tidak pernah lurus karena
tinggi knot tiap karakter diundi dari pita tinta karakter itu sendiri;
parameter kurva (ketebalan, opacity, arah) diundi dalam batas aman. Jawaban
tidak pernah keluar server dalam bentuk apa pun: yang disimpan hanyalah tag
HMAC-SHA-256 atas jawaban ternormalisasi (huruf besar, tanpa whitespace)
di bawah label migo-captcha-v2 dari root MacKey yang sama dengan token lain,
dan migrasi 0002 sudah berbentuk tag sejak awal sehingga tidak ada migrasi
baru; perbandingan constant-time, sekali-jalan per id lewat consume atomik di
store (InMemory untuk satu proses; Postgres mengikuti pola backend lain),
TTL 120 detik, dan verifikasi case-insensitive serta whitespace-insensitive.
Mode image_alt adalah jalur aksesibel: tantangan baru dengan kode acak
berbeda dan render lebih lunak (glyph lebih besar, rotasi lebih kecil, kurva
lebih tipis, kontras tetap) — tetap gambar yang harus dipecahkan, bukan
bypass, karena membacakan jawaban akan membuka gerbang untuk script juga.
Konfigurasi di CaptchaConfig (enabled, length_min/max, ttl_seconds,
accessible_mode, noise_strength 1-5, image_width/height) tervalidasi di
startup dan default-nya ON; auth.captcha_threshold tetap mengatur kapan
gerbang menuntut bukti. Renderer bergantung pada image (png) dan ab_glyph;
answer hanya keluar lewat pintu issue_for_test di balik fitur test-internal
yang tidak pernah dinyalakan jalur produksi, dan 16 test crate ini menutup
alfabet, keunikan gambar dan tag antar-issue, determinisme dari seed,
PNG yang valid dan berukuran konfigurasi, mode alt yang berbeda, verifikasi
benar/salah/kadaluarsa/replay, dua jawaban berlomba yang hanya satu menang,
dan view yang tidak memuat jawaban di respons REST-nya; auth-flow menambah pin wire
(gambar PNG ter-encode dalam field image, tanpa field question, mode alt
berbeda, mode tak dikenal ditolak) dan desktop egui menampilkan tantangan sebagai texture dengan
normalisasi yang sama.

migo-calls, yaitu state machine signaling panggilan (section 165 dan 180): Callkeeper dengan
11 metode yang mengelola siklus hidup Ringing-Connecting-Connected-Ended beserta enam alasan
Ended, invite ber-idempotensi call_id dari client (retry tidak membunyikan dua kali,
reuse dengan payload berbeda dijawab IDEMPOTENCY_MISMATCH), relay SDP dan ICE tersegel yang
server hanya membaca header routing tanpa pernah membuka byte tersegel, gate panggilan
(keanggotaan conversation dan status block dari store, gagal ke arah menolak), sweep invite
kedaluwarsa yang dijalankan di dalam invite sehingga tidak butuh background task, dan
metrik per-state. Store in-memory; TURN dari config (daftar kosong untuk sekarang).
SFU group call (237-238) dijawab FEATURE_DISABLED sampai deployment SFU tersedia.
21 test yang menutup siklus hidup penuh, idempotensi, relay, gate, dan sweep.

tools/protocol-codegen, yaitu generator dan pemeriksa staleness
tools/entity-codegen, yaitu generator entity SeaORM dari server/migrations dan pemeriksa staleness yang dijalankan lewat make entity-check, sehingga schema tetap menjadi satu sumber kebenaran dan entity tidak pernah boleh diedit tangan. Komentar pada file migration menjadi doc comment pada entity, dan komentar yang ditulis di ujung baris sebuah kolom melekat pada kolom yang ditulisi komentar itu, bukan pada kolom berikutnya. Sebelumnya melekat pada kolom berikutnya, sehingga 13 kolom di seluruh schema membawa penomoran enum milik kolom lain, yaitu doc comment yang bukan sekadar hilang melainkan salah dan menjelaskan kolom yang berbeda dari tempatnya duduk
tools/chatbot, yaitu dua akun TypeScript yang dibangun di atas @migo/sdk: setelah register pada satu node, masing-masing membuka koneksi gateway sendiri, lalu keduanya subscribe ke satu conversation langsung, dan pertukaran teks sepuluh round-trip dihitung end-to-end. Test tidak pernah menghitung angka test case di Rust: tujuannya membuktikan bahwa satu node yang berdiri sendiri dapat meng-handle seluruh round-trip login, subscribe, send, dan receive. Server hanya menghitung bot sebagai smoke jalan, bukan test integrasi.
tools/2node, yaitu dua migod berdiri sendiri yang dijalankan bersamaan, masing-masing dengan port HTTP/WS, database postgres, dan node identity sendiri, dipakai untuk smoke test end-to-end dan sebagai template untuk run.sh lokal. Tidak ada mesh federation yang diaktifkan di sini, yaitu setiap node berdiri sendiri dan client memilih satu. Mesh handshake, allow-list, dan region routing adalah pekerjaan yang sudah jadi di migo-federation dan diuji di server/crates/migo-federation/tests/. Skrip run.sh membuat database, menulis file konfigurasi per-test, menunggu kedua node healthy, lalu menjalankan chatbot 10 round-trip. Diperlukan agar setiap commit dapat mengkonfirmasi bahwa alur client dari nol sampai pesan tervalidasi benar-benar bekerja, bukan hanya unit test yang tidak pernah menyentuh soket.
tools/loadgen, yaitu generator beban yang menggerakkan banyak MigoClient nyata lewat jalur SDK yang sama dan melaporkan throughput, persentil latency p50 p90 p99, dan error per kelas dengan menahan diri saat server meminta backoff, lulus tsc --build serta eslint dan prettier, dan 84 test yang menutup aritmetika yang laporannya bergantung padanya yaitu persentil nearest-rank yang dihitung tangan pada n satu, dua, empat, lima, dan sepuluh serta digest kosong yang nol dan bukan NaN, klasifikasi error yang mengambil symbol dan bukan message sebuah penolakan server, pool yang tidak pernah melampaui batas konkurensinya dan menuntaskan setiap item tepat sekali, pacing yang berhenti tepat pada saat interupsi dan menghormati backoff yang diminta server, laporan yang deterministik dan dapat diparse sebagai JSON, dan redaksi yaitu userinfo yang dilepas dari url, oktet terakhir IPv4 yang ditutup, host IPv6 yang diredaksi, nilai di bawah key yang tampak rahasia beserta bearer token yang ditutup, dan seluruh tulisan logger yang pergi ke stderr dan bukan stdout
shared/protocol/schema, yaitu IDL lengkap dengan 168 struct, 15 enum, 100 opcode, dan 70 error code
shared/protocol/vectors, yaitu 6 file test vector biner untuk varint, frame, MSE, HKDF, AEAD, dan HMAC, dijalankan oleh 21 test Rust di migo-wire dan migo-crypto, dengan nilai harapan yang tidak boleh berasal dari output implementasi ini sendiri melainkan dihitung tangan dari spesifikasi atau oleh implementasi independen di tools/vectors, dan generator crypto menolak menulis file sebelum berhasil mereproduksi vector resmi RFC 5869, RFC 4231, dan draft-irtf-cfrg-xchacha. Sisi TypeScript membaca file yang sama dan dijalankan oleh 48 test di packages/wire, packages/protocol, dan packages/crypto, sehingga make test-vectors sekarang menjalankan kedua sisi dan tidak lagi bisa lulus dengan hanya satu bahasa yang diperiksa
tools/vectors, yaitu dua generator independen dan pemeriksa staleness yang dijalankan lewat make vector-check tanpa memerlukan toolchain Rust sehingga dapat berada di gate cepat CI
.github/workflows/ci.yml, yaitu gate wajib pada setiap pull request yang menjalankan make ci dengan PostgreSQL dan Redis sebagai service sehingga contract suite benar-benar berjalan, flag MIGO_TEST_REQUIRE_BACKENDS yang membuat suite yang tidak terkonfigurasi gagal alih-alih lulus tanpa menyentuh backend, gate doc-check yang menolak intra-doc link rusak karena cargo test hanya mengompilasi contoh dokumentasi dan tidak pernah menyelesaikan tautannya, job MSRV terpisah yang memverifikasi janji rust-version 1.94, yaitu angka yang ditetapkan oleh sea-orm 2.0 dan bukan pilihan proyek ini, dan job audit yang melaporkan advisory tanpa memblokir merge dan membawa tepat satu ignore beralasan di server/.cargo/audit.toml, yaitu RUSTSEC-2026-0235 pada rkyv yang masuk sebagai feature opsional rust_decimal yang tidak pernah dinyalakan sehingga tercatat di Cargo.lock tanpa pernah dikompilasi, karena job advisory yang merah pada setiap commit mengajari semua orang melewatinya dan itu justru kebalikan dari gunanya, ditambah job gates yang berjalan tanpa toolchain Rust sama sekali dan menjalankan ketujuh pemeriksaan yang cukup membaca berkas, yaitu make protocol-check, make entity-check, make brief-check, make vector-check, make kotlin-check, make infra-check, dan make pydeps-check yang memastikan baris pip pada job itu memasang tepat modul pihak ketiga yang diimpor tools/ karena menyematkan interpreter juga menyembunyikan apa pun yang kebetulan sudah ada di image runner, sehingga kesalahan yang dapat dilihat tanpa mengompilasi apa pun dilaporkan dalam satu menit dan bukan setelah seluruh workspace dibangun

KODE LENGKAP, TEST BELUM DITULIS:

Blok ini ada supaya penanda BUILT tidak pernah dilonggarkan, dan pada saat ini blok itu kosong. Keenam crate yang pernah berada di sini sudah berpindah ke BUILT bersama test integrasinya masing-masing, sehingga setiap anggota server/Cargo.toml kini memiliki test yang dijalankan CI. Blok ini sengaja tidak dihapus, karena ia adalah tempat yang benar bagi crate berikutnya yang kodenya selesai lebih dulu daripada test-nya, dan aturan pemeliharaan di akhir section ini menuntutnya kosong pada saat rilis.

SCHEMA, sudah di IDL dan codegen:

NOTIFICATION_EVENT bernomor 144 kini punya tiga penerbit nyata: permintaan dan penerimaan pertemanan (lewat Notice dari migo-social yang diteruskan dispatcher ke notifier dan ke topik User penerima), hadiah yang tiba (lewat Announcer ekonomi yang diikat composition root ke notifier, plus pencerminan realtime dari dispatcher gift), dan jalur manual Gateway::emit_notification yang tetap ada untuk penerbit di luar sesi; FRIEND_EVENT 115 dipublikasikan dispatcher sosial ke topik User penerima bersama notifikasinya, ECONOMY_EVENT 162 dipublikasikan ke topik User pengirim untuk live balance tick, dan MEDIA_STATE_EVENT 133 dipublikasikan handler commit ke topik Conversation yang menampung objek dengan coalescing per object, sehingga seluruh seratus opcode IDL kini menyentuh kabel dan setiap opcode server-to-client yang dimiliki domain punya penerbit
Feature bit 0 sampai 15, yaitu konstanta yang sudah di-codegen dan dinegosiasikan handshake tetapi belum satu pun yang mengubah perilaku server, kecuali BandwidthMode yang bukan feature bit namun kini dibaca gateway dari HELLO, disimpan pada session handle, dan sampai ke Caller presence sehingga kadensi heartbeat menurut mode (section 75)

SPEC, baru ada di dokumen:

Metadata block pada section 141 dan flag bit 0x40
Requirement produk voice note pada section 179 dan requirement produk call pada section 180

Sudah meninggalkan SPEC dan menyentuh kabel: opcode messaging 40 sampai 42 (edit, reaksi), profile 111 dan 112, social 113 sampai 119 (termasuk suggestions dan search), room 80 sampai 89 (termasuk create, roster, role, update, archive), media 128 sampai 133, economy 160 sampai 167 (termasuk catalogue, ledger, progression, badges, leaderboard), games 176 sampai 186 (termasuk start, view, abandon, catalogue), dan call 224 sampai 238 (kini dengan data plane HTTP di migo-api untuk backend filesystem dan scan inline pada commit sehingga media non-E2E tidak lagi terkunci Pending), notification 145 dan 146 beserta penerbitnya, economy 160 sampai 162, bot 178 sampai 180, moderation 192 sampai 194, dan federation 208 sampai 221 dengan transport mesh di migod; feature bit 16 sampai 20 dinegosiasikan dan federasi memakainya

KODE LENGKAP DI LUAR WORKSPACE CARGO, TEST BELUM DITULIS:

Blok ini memegang peran yang sama seperti blok crate di atas tetapi bagi kode di luar workspace Cargo server. Tiga dari lima item yang pernah berada di sini yaitu SDK TypeScript, client web, dan generator beban sudah berpindah ke BUILT bersama test-nya masing-masing, sehingga yang tersisa adalah dua item yang belum memiliki test yang menjalankan kodenya sendiri: client desktop native yang lulus cargo clippy dengan --all-targets tanpa satu pun peringatan tetapi belum memiliki satu pun test, dan berkas deployment yang sudah memiliki gate statis tetapi belum memiliki smoke test yang benar-benar menyalakan stack-nya. Menandai keduanya BUILT akan melanggar aturan pemeliharaan di akhir section ini sedangkan meninggalkannya di BELUM ADA KODE akan membuat orang menulis ulang kode yang sudah ada, dan keduanya hanya boleh berpindah ke arah BUILT.

clients/desktop, yaitu client native di atas eframe dan egui dengan 16 berkas Rust dan sekitar 5800 baris yang menaut migo-core, migo-wire, migo-protocol, dan migo-crypto lewat path dan bukan lewat crates.io, sehingga codec wire dan kriptografinya benar-benar satu salinan dengan server dan bukan implementasi kedua yang dapat menyimpang darinya. Ia adalah workspace Cargo tersendiri dan sengaja bukan anggota workspace server, karena eframe menarik winit, glutin, tumpukan font, dan pohon accessibility, sedangkan sebuah anggota akan memasukkan semua itu ke Cargo.lock server serta ke setiap cache CI server padahal server dimaksudkan tetap cukup kecil untuk dibangun pada VPS sederhana; dua workspace membuat kedua graf dependensi dan kedua lockfile terpisah sambil tetap berbagi satu salinan bagian yang justru tidak boleh bercabang, dan dependensi path tetap mewarisi workspace-nya sendiri sebab migo-core dan saudaranya menyelesaikan version.workspace terhadap Cargo.toml server yang merupakan leluhurnya dan bukan terhadap berkas ini. Backend render adalah glow dan bukan wgpu default karena ini jendela chat dan bukan renderer, GL 3.3 tersedia pada setiap mesin yang dapat menjalankan browser, dan binary hasilnya menyala pada VM headless dengan driver GL perangkat lunak. TLS-nya rustls dan bukan OpenSSL sistem karena binary rilis yang menaut libssl rusak begitu ia mendarat di distribusi dengan OpenSSL 3 minor yang berbeda, sedangkan messenger justru program yang paling tidak boleh memiliki tumpukan TLS yang berubah menurut host. Brankas di disk memakai Argon2id langsung dari crate teraudit dan bukan lewat migo-crypto::password, sebab yang dibutuhkan brankas adalah kebalikan dari hashing password, yaitu byte kunci mentah dari sebuah passphrase alih-alih string PHC untuk diverifikasi server, dan parameternya disetel di call site jauh di atas biaya login server karena membuka brankas adalah satu hash pada satu device sehingga memori yang akan gegabah di bawah lonjakan login justru gratis di sini. Kunci privat dibangkitkan di device, tidak pernah dikirim ke server, dan tidak pernah keluar dari proses selain sebagai ciphertext brankas, sedangkan cryptographic envelope section 11 dikodekan di src/crypto/envelope.rs dengan layout yang sama field demi field dengan packages/sdk dan clients/android sehingga pesan yang disegel di sini terbuka di ketiganya. Struktur berkasnya memisahkan lapisan dengan tegas, yaitu main.rs yang hanya memasang subscriber log lalu menyerahkan jendela, app.rs sebagai satu-satunya tempat state aplikasi hidup, model.rs untuk tipe tampilan, theme.rs untuk token desain, ui/ untuk widget dan layar, net/ untuk REST dan gateway, crypto/ untuk envelope, session, dan konten, serta vault.rs untuk brankas; render berjalan di thread UI sedangkan setiap I/O berjalan di runtime tokio sehingga tidak ada await yang menahan frame. Profil rilisnya menyalakan lto tipis, satu codegen unit, strip simbol, dan panic abort, karena client desktop dikirim sebagai satu binary kepada orang yang tidak akan pernah membaca backtrace.
infra, yaitu Dockerfile.migod, Dockerfile.web, docker-compose.yml, dan README yang menyusun server, PostgreSQL, dan Redis menjadi satu stack yang dapat dijalankan, yaitu berkas deployment yang divalidasi dengan menjalankan stack dan bukan dengan test unit sehingga tempatnya di sini sampai ada smoke test yang menjalankannya di CI. Sejak commit ini ada satu gate statis yaitu tools/scripts/infra-audit.py yang dijalankan CI sebagai make infra-check dan memeriksa 12 hal yang dapat dibaca dari berkas tanpa penilaian, yaitu setiap image yang dipin ke tag tetap, tidak adanya material kunci privat maupun nilai berbentuk secret di luar konstanta development yang di-allow-list, tidak adanya container privileged, host namespace, maupun mount host yang dapat ditulis, requests dan limits beserta kedua probe pada setiap workload Kubernetes, tidak adanya dua service yang menerbitkan host port yang sama, dan web yang menerbitkan tepat port 19991; gate itu tidak menyalakan satu pun container, sehingga item ini tetap berada di blok ini

KODE LENGKAP, KOMPILASI DIVERIFIKASI DI CI, TEST BELUM DITULIS:

Blok ini memegang kode yang sudah lengkap tetapi tidak dapat dikompilasi di lingkungan tempat ia ditulis, karena lingkungan itu tidak memiliki toolchain JVM maupun Android. Ia dipisahkan dari kedua blok di atas justru karena bukti kompilasinya berada di tempat lain: kedua blok di atas menyatakan lulus kompilasi di mesin penulis, sedangkan yang di sini lulus di runner GitHub, dan menggabungkan keduanya akan membuat satu klaim menanggung dua jenis bukti sehingga pembaca tidak dapat lagi memeriksa yang mana. Item di sini berpindah ke BUILT hanya setelah memiliki test yang lulus, dan tidak pernah ke arah sebaliknya.

clients/android, yaitu SDK Kotlin pada modul :core dan aplikasi Compose pada modul :app. Yang sudah ada: codec wire MWP lengkap yang ditulis tangan untuk frame, varint, MSE, flag, limit, id base32, timestamp epoch Migo, kompresi DEFLATE lewat java.util.zip, dan batching, sejajar dengan packages/wire di sisi TypeScript; lapisan protocol Generated.kt yang di-generate dari shared/protocol/schema oleh tools/protocol-codegen sebagai target ketiga di samping Rust dan TypeScript sehingga make protocol-check menjaga kesegarannya persis seperti kedua sisi lain dan seluruh struct, enum, opcode, error code, feature bit, serta tabel opcode berasal dari satu sumber kebenaran yang sama; modul :app berupa satu Activity Compose yang menaut :core sebagai SDK dan berkompilasi menjadi APK yang dapat dipasang; dan lapisan crypto lengkap di atas Lazysodium dengan 14 berkas untuk AEAD XChaCha20-Poly1305, HKDF berlabel, HMAC-SHA256, MAC, CSPRNG, identity Ed25519 dan X25519, X3DH, Double Ratchet, sender key, cryptographic envelope section 11, dan konten pesan, tanpa satu pun primitive yang ditulis di Kotlin sendiri, dengan label HKDF serta layout envelope yang disalin verbatim dari sisi Rust dan packages/crypto sehingga pesan yang disegel salah satu dari ketiga client terbuka di kedua yang lain, sebab satu byte label yang berbeda di sini membuat client ini tidak lagi dapat membaca pesan yang ditulis client lain meskipun setiap primitive tetap lulus test-nya sendiri. Lapisan transport lengkap pada net dengan dua berkas, yaitu Rest.kt di atas OkHttp untuk keempat endpoint /v1/auth beserta amplop error section 161 yang hanya membawa public_message, dan Gateway.kt untuk sesi WebSocket MWP dengan handshake HELLO, heartbeat PING, reconnect exponential backoff yang menghormati RECONNECT_HINT, serta pembongkaran batch dan DEFLATE; penyimpanan pada store dengan empat berkas, yaitu Keystore.kt yang membangkitkan kunci AES-GCM di dalam Android Keystore, SessionStore.kt yang mengimplementasikan SessionPersistence dan GroupPersistence sehingga ratchet 1:1 dan sender key bertahan melewati restart proses, Vault.kt sebagai brankas material kunci, dan Settings.kt di atas DataStore untuk preferensi yang bukan rahasia; state kriptografis pada session dengan dua berkas, yaitu SessionCrypto.kt untuk X3DH dan Double Ratchet per device serta GroupCrypto.kt untuk sender key per percakapan; MigoClient.kt beserta tiga belas berkas domain untuk keys, messaging, conversations, sync, typing, presence, rooms, profile, notifications, games, rpc, listeners, dan errors, yang bersama-sama meneruskan kedua puluh satu opcode arah client ke server; serta modul :app berupa aplikasi Compose sesungguhnya dengan sepuluh berkas, yaitu MigoSession yang menyatukan brankas, penyimpanan ratchet, dan client menjadi satu kesatuan yang dibuat bersama atau tidak sama sekali, AppViewModel sebagai satu-satunya StateFlow state aplikasi, tema, layar sign in, daftar percakapan, dan layar chat. Sesuai section 164, tidak ada private key yang berada di SharedPreferences, berkas biasa, log, maupun backup OS: kunci pembungkus dibangkitkan di dalam Keystore, material kunci untuk primitive yang tidak didukung Keystore hanya ada di disk dalam keadaan tersegel di bawah kunci itu, dan plaintext-nya hanya ada di memori selama operasi. Yang masih SPEC dan karena itu belum tampak di UI: pencarian direktori pengguna, sebab opcode social 113 sampai 117 belum ada, sehingga memulai percakapan baru dilakukan dengan menempelkan account id dan bukan dengan mencari nama; rendering media, sebab section 168 masih SPEC dan server memang tidak pernah menjadi proxy byte, sehingga avatar digambar sebagai monogram; serta panggilan suara dan video, sebab section 165 dan 166 masih SPEC. Yang juga belum ada dan bukan SPEC melainkan keputusan: tidak ada basis data pesan lokal, sehingga plaintext hanya hidup di dalam state chat yang sedang terbuka dan menutup chat membuangnya, karena penyimpanan pesan yang bertahan wajib tersegel di bawah kunci Keystore persis seperti ratchet-nya, dan menambahkannya setengah jalan berarti menaruh plaintext percakapan di disk tanpa segel itu. Karena sandbox tidak memiliki toolchain Android, kode ini tidak pernah dikompilasi secara lokal, dan workflow .github/workflows/android.yml mengompilasi :core dan :app serta menjalankan unit test SDK di runner GitHub pada setiap push dan pull request sedangkan .github/workflows/release.yml membangun APK sebagai aset rilis pada setiap tag versi, sehingga kompilasi yang tidak dapat dibuktikan di sini dibuktikan di sana. Test-nya sendiri, yaitu vektor konformansi yang membuktikan bahwa envelope yang disegel Kotlin terbuka di Rust dan TypeScript, ditulis pada tahap konsolidasi test yang sama dengan blok di atas.

BELUM ADA KODE:

tests/e2e

Aturan pemeliharaan bagian ini:

Status WAJIB diperbarui pada commit yang sama dengan perubahan kodenya. Status yang basi lebih buruk daripada tidak ada status, karena membuat orang percaya pada sesuatu yang tidak ada
Sebuah item hanya boleh ditandai BUILT bila memiliki test yang lulus, bukan hanya karena berhasil dikompilasi
Ketiga blok yang namanya memuat TEST BELUM DITULIS bukan kelonggaran atas aturan di atas melainkan penegakannya. Ketiganya WAJIB kosong pada saat rilis, dan sebuah item hanya boleh berpindah keluar dari blok itu ke arah BUILT, tidak pernah ke arah sebaliknya


178. DOCUMENT AUDIT AND CONSISTENCY RULES

STATUS: BUILT untuk pemeriksaan otomatis di tools/scripts/brief-audit.py, yang dijalankan lewat make brief-check dan menjadi bagian dari make ci. STATUS: SPEC untuk pemeriksaan yang masih dilakukan manusia.

Dokumen ini WAJIB diaudit setiap kali arsitektur berubah. Audit berikut dijalankan dan hasilnya harus bersih.

Pemeriksaan yang dapat diotomatisasi:

Tidak ada penyebutan JSON sebagai wire protocol realtime. Setiap penyebutan JSON WAJIB berada dalam konteks REST, konfigurasi, admin, log, test fixture, atau IDL
Tidak ada penyebutan MessagePack, CBOR, atau base64 sebagai format realtime
Tidak ada penyebutan polling, long polling, atau setInterval sebagai mekanisme pengambilan data realtime
Tidak ada penyebutan penyimpanan private key di localStorage, sessionStorage, atau cookie
Tidak ada klaim end-to-end untuk Public Room atau Managed Room
Nomor section berurutan dan tidak ada nomor ganda
Setiap referensi antar section menunjuk ke section yang ada
Setiap opcode yang disebut di dokumen ada di section 145
Setiap error symbol yang disebut di dokumen ada di shared/protocol/schema/errors.json
Setiap nama limit yang disebut ada di shared/protocol/schema/meta.json
Setiap section protocol memiliki penanda status
Setiap nama SCREAMING_SNAKE_CASE yang dipakai di dokumen dapat dilacak ke schema atau ke salah satu section registry, yaitu section 48 untuk permission produk, section 72 untuk feature bit, section 140 untuk frame flag, section 145 untuk opcode, dan section 161 untuk error code. Aturan ini yang mencegah munculnya nama yang dikarang di tengah prosa
Response tidak ditulis sebagai opcode SCREAMING_SNAKE_CASE, karena response tidak memiliki opcode sendiri
Setiap enum di enums.json diacu dengan nama schema-nya, bukan hanya dideskripsikan dengan kata-kata
Setiap opcode di opcodes.json muncul di daftar section 145
Tidak ada markdown heading, bullet, backtick, bold, atau trailing whitespace, karena dokumen ini ditulis sebagai teks datar bernomor
Setiap crate yang terdaftar sebagai anggota workspace di server/Cargo.toml muncul di daftar BUILT pada section 177
Tidak ada crate yang sekaligus tercantum sebagai BUILT dan sebagai BELUM ADA KODE pada section 177, karena status yang bertentangan membuat kedua baris terlihat benar

Pemeriksaan di atas dijalankan oleh tools/scripts/brief-audit.py lewat make brief-check, dan make ci akan gagal bila salah satu tidak bersih. Skrip itu sendiri diuji dengan cara merusak salinan dokumen secara sengaja lalu memastikan skrip menolaknya; pemeriksa yang tidak pernah gagal tidak membuktikan apa pun.

Pemeriksaan konsistensi lintas dokumen:

Angka limit di dokumen ini WAJIB sama dengan meta.json
Daftar feature bit WAJIB sama dengan meta.json
Daftar opcode yang bertanda SCHEMA WAJIB sama dengan opcodes.json
Backoff, heartbeat, dan jendela resume WAJIB sama dengan docs/02-protocol.md
Budget bandwidth WAJIB sama dengan docs/05-bandwidth-budget.md
Requirement produk call pada section 180 WAJIB konsisten dengan protokolnya pada section 165 dan section 166
Requirement produk voice note pada section 179 WAJIB konsisten dengan protokolnya pada section 167
Referensi "brief §NN" di docs WAJIB tetap menunjuk ke section yang benar, sehingga penomoran section 1 sampai 135 tidak boleh diubah

Aturan resolusi konflik, sama dengan section 0:

Untuk byte di kabel, shared/protocol/schema menang
Untuk protocol semantics, section 136 sampai 178 menang
Untuk produk dan fitur, section 1 sampai 135 dan section 179 ke atas menang

Bila audit menemukan pertentangan, yang diperbaiki adalah dokumennya, bukan kodenya, kecuali kode itu sendiri yang melanggar requirement. Dokumen yang tidak akurat akan diikuti orang, dan itu lebih berbahaya daripada dokumen yang kosong.

179. VOICE NOTE PRODUCT REQUIREMENT

STATUS: SPEC. Spesifikasi protokolnya ada di section 167, arsitektur media di section 168, dan target bandwidth di section 171. Bagian ini adalah requirement produknya. Letaknya di akhir dokumen karena penomoran section 1 sampai 135 dibekukan, bukan karena prioritasnya rendah.

Voice note adalah pesan audio asynchronous: direkam, dikirim, lalu didengar kapan saja. Voice call adalah percakapan realtime. Keduanya sering dianggap satu fitur dan itu keliru, karena tuntutan tekniknya berlawanan. Voice note dioptimalkan untuk ukuran dan keandalan pengiriman, boleh tertunda, dan WAJIB bertahan melewati aplikasi yang ditutup. Voice call dioptimalkan untuk latensi dan kehilangan maknanya bila tertunda satu detik. Voice note memakai jalur pesan pada section 167, voice call memakai jalur signaling pada section 180.

Voice note WAJIB tersedia di private chat, group chat, Public Room, dan Managed Room, tunduk pada permission room. Voice note bukan fitur dengan layar sendiri, melainkan satu jenis pesan di dalam composer yang sama, sehingga pengguna berpindah antara teks, voice note, voice call, dan video call tanpa berpindah konteks.

Kemampuan yang WAJIB ada:

Record, send, receive
Play, pause, seek, replay
Playback speed
Waveform dan durasi
Download dan cache
Delete, forward, reply, quote, react, pin
Mark as listened dan unlistened

Interaksi perekaman. Dua mode WAJIB tersedia, karena keduanya melayani situasi berbeda dan memaksa satu mode saja akan membuat sebagian pengguna kehilangan rekaman:

Mode tekan-tahan
Tahan tombol microphone untuk merekam, geser untuk membatalkan, lepas untuk mengirim. Cepat untuk pesan pendek.

Mode dua langkah
Tap microphone untuk mulai, tap stop untuk berhenti, lalu preview dengan pilihan send atau delete. Diperlukan untuk pesan panjang dan untuk pengguna yang tidak dapat menahan tombol lama.

Lock recording
Dari mode tekan-tahan, geser ke atas untuk mengunci. Setelah terkunci, perekaman berjalan tanpa tombol ditahan dan tersedia pause, resume, cancel, serta send.

Kontrol saat merekam:

Timer berjalan
Waveform live
Pause dan resume
Cancel
Send
Delete pada mode preview

Contoh tampilan:

Recording...

00:18

[ Pause ]  [ Cancel ]  [ Send ]

Aturan perekaman yang WAJIB dipenuhi:

Rekaman ditulis bertahap ke penyimpanan privat aplikasi selama perekaman berjalan, bukan ditahan seluruhnya di memori. Rekaman lima menit yang hanya ada di memori akan hilang ketika sistem operasi menutup aplikasi, dan itu terjadi tepat pada perangkat murah yang paling banyak dipakai
Cancel TIDAK BOLEH menghapus rekaman secara langsung. Rekaman disimpan sebagai draft yang dapat dipulihkan lewat Undo selama beberapa detik, karena geser-batal yang tidak disengaja adalah kesalahan yang paling sering dilakukan pengguna pada mode tekan-tahan
Interupsi yaitu panggilan masuk, layar terkunci, aplikasi berpindah ke background, atau kehilangan audio focus WAJIB memicu pause lalu resume, bukan pembatalan
Bila aplikasi mati saat merekam, draft WAJIB muncul kembali di composer conversation yang sama saat aplikasi dibuka
Permission microphone diminta saat pengguna pertama kali menekan record, TIDAK BOLEH saat aplikasi atau halaman dibuka. Pada Android permission itu adalah RECORD_AUDIO
Penolakan permission dijelaskan sekali dengan cara memperbaikinya, dan tombol record tetap terlihat dalam keadaan nonaktif, bukan hilang tanpa penjelasan

Kualitas audio. Voice note memakai codec speech, dan pilihan kualitas mengikuti enum BandwidthMode pada section 75 supaya pengguna tidak mengatur hal yang sama di dua tempat:

Normal
Kualitas suara baik dengan ukuran file kecil. Ini default.

LowData
Bitrate lebih rendah, durasi maksimum tetap.

UltraLowData
Bitrate paling rendah dan auto-download dimatikan sepenuhnya.

Kualitas tinggi bersifat OPSIONAL dan hanya boleh dipilih pengguna secara sadar. Format lossless besar TIDAK BOLEH menjadi default, karena voice note adalah suara manusia dan bukan musik. Optimasi silence diperbolehkan bila codec mendukungnya tanpa merusak kejelasan bicara.

Batas durasi:

Default 5 menit
Dapat dikonfigurasi sampai 10 menit oleh operator
Batas diperiksa di server pada MEDIA_UPLOAD_COMMIT, bukan hanya di client, karena batas yang hanya ada di client bukan batas
Rekaman yang melewati batas ditolak dengan UPLOAD_LIMIT_EXCEEDED, dan client menampilkan batas yang dilanggar, bukan pesan gagal yang generik

Bila rekaman melewati batas, client memperingatkan pengguna sebelum upload dimulai. Room dapat menetapkan batas lebih pendek melalui policy-nya, dan client WAJIB menampilkan batas yang berlaku sebelum pengguna merekam, bukan setelah gagal mengirim.

Playback:

Kecepatan 0.5x, 1x, 1.5x, dan 2x, dengan default 1x
Kecepatan dipilih di client tanpa meminta ulang media
Posisi terakhir WAJIB diingat per pesan, sehingga voice note panjang yang terputus dilanjutkan dan tidak dimulai dari awal
Playback berlanjut ketika layar terkunci, ketika pengguna membuka aplikasi lain, dan ketika pengguna berpindah halaman
Antrean putar otomatis untuk voice note berurutan dari pengirim yang sama bersifat OPSIONAL dan WAJIB dapat dimatikan
Audio session mengikuti aturan sistem operasi, termasuk audio focus, ducking, dan interupsi oleh panggilan

Output audio yang WAJIB didukung: speaker, earpiece, Bluetooth, dan wired headset. Perpindahan output saat playback berjalan tidak memutus playback. Bila platform menyediakan proximity sensor, mendekatkan perangkat ke telinga dapat memindahkan output ke earpiece.

Auto-download. Setting per akun dengan empat nilai:

Never
Wi-Fi only
Wi-Fi dan mobile data
Auto, yaitu mengikuti BandwidthMode

Default adalah Wi-Fi only. Pada LowData dan UltraLowData auto-download dimatikan dan voice note diunduh saat pengguna menekan play. Alasannya bukan penghematan server, melainkan bahwa kuota adalah biaya nyata bagi pengguna yang paling banyak memakai Migo.

Cache. Voice note yang sudah diputar disimpan agar tidak diunduh ulang:

Cache menyimpan hasil dekripsi di penyimpanan privat aplikasi dalam keadaan terenkripsi at rest oleh kunci perangkat
Batas ukuran dapat dipilih pengguna, misalnya 50 MB, 100 MB, 500 MB, atau 1 GB
Penghapusan otomatis dapat dipilih: 1 hari, 7 hari, 30 hari, atau tidak pernah
Penghapusan cache TIDAK BOLEH menghapus pesan dari server, dan UI WAJIB menyatakannya, karena "clear cache" yang menghapus riwayat adalah kejutan yang tidak dapat dibatalkan
Eviksi memakai least-recently-used dengan pengecualian pesan yang di-pin

Listen once. Fitur OPSIONAL untuk private chat: setelah selesai diputar satu kali, audio menjadi tidak tersedia dan object di storage dihapus oleh job pembersih.

Batasannya WAJIB dinyatakan apa adanya di UI: penerima secara teknis dapat merekam audio dengan perangkat lain atau menyalin file sebelum diputar. Fitur ini mengurangi jejak, bukan menjamin kerahasiaan. Klaim yang lebih kuat dari itu adalah klaim palsu.

Aksi pada pesan voice note:

Reply, dengan kutipan yang menampilkan referensi ke voice note asli beserta durasinya, misalnya "Voice note 00:32"
Quote di dalam pesan teks
React memakai reaction kecil sesuai section 59, bukan pesan baru
Pin sesuai permission room
Delete untuk diri sendiri atau untuk semua orang sesuai permission
Forward ke friend, group, Public Room, dan Managed Room, selalu melalui pemeriksaan permission di tujuan

Forward memiliki konsekuensi metadata yang WAJIB dipahami. Object ciphertext yang sama dapat dipakai ulang sehingga forward tidak mengunggah audio dua kali, dan itu sesuai dedup pada section 69. Konsekuensinya server melihat media_id yang sama muncul di dua conversation dan dapat menyimpulkan bahwa forward terjadi, meski tetap tidak dapat mendengar isinya. Karena itu:

Forward di dalam room dan group memakai object yang sama, karena penghematan bandwidth-nya besar dan keanggotaannya sudah diketahui server
Untuk private chat WAJIB tersedia pilihan forward yang mengenkripsi ulang dengan kunci media baru lalu mengunggah ulang, sehingga tidak ada media_id yang sama di dua conversation
Pilihan mana yang dipakai WAJIB terlihat oleh pengguna, bukan diputuskan diam-diam

Status pengiriman. Voice note memiliki tahap lebih banyak daripada pesan teks, dan menyembunyikan tahap itu membuat pengguna menekan kirim berulang kali:

Recording
Processing, yaitu encode dan hitung waveform
Uploading, dengan persentase
Sending
Sent
Delivered
Played
Failed, dengan sebab dan tombol retry

Contoh tampilan: satu centang untuk Sent, dua centang untuk Delivered, dan penanda speaker untuk Played.

Delivered memakai MESSAGE_RECEIPT dengan ReceiptKind bernilai Delivered. Played belum memiliki nilai di ReceiptKind, sehingga menampilkannya ke pengirim menuntut penambahan varian pada enum tersebut di shared/protocol/schema. Selama varian itu belum ada, Played hanya boleh menjadi status lokal pada perangkat penerima dan TIDAK BOLEH ditampilkan sebagai status terkonfirmasi di sisi pengirim. Menampilkan status yang tidak pernah dikirim adalah berbohong kepada pengguna.

Mark as listened dan unlistened adalah keadaan lokal penerima. Menandai unlistened tidak membatalkan receipt yang sudah dikirim.

Waveform dihitung di client saat merekam, sebelum enkripsi, dan dikirim sebagai array bucket berjumlah tetap di dalam envelope terenkripsi. Server tidak dapat menghitungnya dan memang tidak boleh. Batas ukurannya ada di section 171.

Contoh bentuk yang ditampilkan:

.:il|Il:.,:iI|li:.

Transkripsi dan translation bersifat OPSIONAL:

Untuk voice note E2E, transkripsi WAJIB dilakukan di perangkat
Bila operator menyediakan transkripsi di server, fitur itu WAJIB dinyatakan tidak end-to-end, WAJIB meminta izin eksplisit per fitur dan bukan lewat persetujuan umum, dan WAJIB dapat dimatikan
Hasil transkripsi dapat dilihat, disalin, diterjemahkan, dan dicari
Translation memakai jalur section 35 dengan transcript sebagai masukan
Pengguna dapat memilih menampilkan audio asli, transcript, atau translation

Search voice note berdasarkan pengirim, tanggal, conversation, dan durasi. Bila transkripsi diaktifkan, transcript dapat dicari. Untuk conversation E2E, index pencarian berada di perangkat. Server TIDAK BOLEH menerima transcript plaintext hanya untuk keperluan index.

Voice note di room. Public Room dan Managed Room mengatur voice note melalui permission pada section 48, yaitu VOICE_NOTE_SEND, VOICE_NOTE_DELETE, VOICE_NOTE_FORWARD, dan VOICE_NOTE_PLAY. Manager memilih salah satu kebijakan:

Allowed untuk semua anggota
Members only, yaitu anggota terdaftar saja
Verified users only
Disabled

Permintaan yang tidak diizinkan dijawab PERMISSION_DENIED. Voice note di Public Room dan Managed Room dapat dibaca server dan WAJIB melewati moderation pada section 49; itu bukan jalur end-to-end dan perbedaan ini WAJIB terlihat di UI sesuai section 59.

Anti-spam. Rate limit voice note memakai empat dimensi sekaligus, karena membatasi jumlah pesan saja masih membuka pengiriman rekaman panjang berulang-ulang:

Jumlah voice note per menit
Total durasi audio per menit
Bandwidth upload per jendela waktu
Jumlah upload yang belum di-commit secara bersamaan

Pelanggaran dijawab RATE_LIMITED dengan retry_after. Detail arsitektur rate limit ada di section 120.

Upload dan offline. Keduanya sudah menjadi requirement wajib pada section 167, dan di level produk berarti:

Antrean voice note bersifat durable di perangkat, sehingga rekaman yang dibuat saat offline tidak hilang karena aplikasi ditutup
Saat offline, pesan menampilkan "Waiting for connection", bukan Failed
Upload dapat di-pause, resume, retry, dan cancel, dan kegagalan di tengah dilanjutkan dari posisi terakhir melalui MEDIA_UPLOAD_STATUS
Upload yang gagal berkali-kali berhenti mencoba dengan backoff, tidak menghabiskan baterai

Implementasi Android:

Kotlin dan Jetpack Compose
Perekaman memakai API audio resmi Android sesuai versi OS
Audio focus dan media session ditangani, termasuk saat playback di background
Penyimpanan lokal terenkripsi dengan kunci dari Android Keystore, sesuai section 109 dan section 164
Foreground handling hanya selama perekaman atau upload berjalan, dan TIDAK BOLEH ada service permanen ketika tidak ada aktivitas

Implementasi Web:

Next.js dengan MediaRecorder dan Web Audio untuk perekaman serta visualisasi waveform
Penyimpanan draft dan cache di IndexedDB, kunci non-extractable lewat Web Crypto sesuai section 164
Service Worker dipakai bila diperlukan untuk melanjutkan upload
Permission microphone diminta saat pengguna menekan record

Arsitektur iOS WAJIB tetap kompatibel dengan protocol yang sama, karena keputusan yang dibuat sekarang menentukan apakah client ketiga mungkin ditulis tanpa mengubah wire format.

Yang diketahui server tentang sebuah voice note terbatas pada message_id, conversation_id, kind bernilai Voice dari enum MessageKind, pengirim, seq, created_at, media_id, ukuran object, dan waktu upload. Durasi, sample rate, codec, dan waveform berada di dalam ciphertext. Metadata di luar daftar itu TIDAK BOLEH ditambahkan tanpa alasan yang ditulis, karena metadata yang tidak diperlukan adalah kebocoran yang tertunda.

Push notification untuk voice note hanya menyebut adanya pesan suara baru. Plaintext audio, transcript, dan durasi yang bersifat sensitif TIDAK BOLEH masuk ke payload push. Lihat section 77.

180. VOICE AND VIDEO CALL PRODUCT REQUIREMENT

STATUS: SPEC. Protokol signaling ada di section 165, arsitektur media di section 166, dan target bandwidth di section 171. Bagian ini adalah requirement produknya, ditempatkan di akhir dokumen karena penomoran section 1 sampai 135 dibekukan.

Model dasar Migo untuk panggilan adalah P2P ditambah E2E sebagai default. Keduanya bukan alternatif dan sering tertukar: P2P menentukan jalur yang dilewati media, E2E menentukan siapa yang dapat membacanya. Panggilan yang P2P tanpa E2E tetap dapat dibaca pihak yang menyisipkan diri di jalur, dan panggilan yang E2E tanpa P2P tetap membebani server dengan seluruh bandwidth media. Migo memakai keduanya, sehingga server Migo tidak pernah menjadi pihak yang dapat mendengar atau melihat panggilan private.

Feature bit yang mengatur ketersediaannya adalah CALL_V1 untuk 1-on-1 dan GROUP_CALL_SFU_V1 untuk group, keduanya pada section 72. Ketika bit tidak dinegosiasikan, tombol call tidak ditampilkan dan permintaan dijawab FEATURE_NOT_NEGOTIATED.

Voice call, kemampuan yang WAJIB ada:

Panggilan 1-on-1 dan group
Incoming, outgoing, missed call
Call history dan durasi
Call notification
Call waiting
Accept, reject, end
Mute microphone
Speaker, earpiece, Bluetooth, wired headset, dan perpindahan output saat panggilan berjalan
Background call dan kontrol pada lock screen
Reconnect otomatis
Indikator kualitas jaringan
Mode audio bitrate rendah dan bitrate adaptif
Echo cancellation, noise suppression, automatic gain control

Video call, kemampuan yang WAJIB ada:

Panggilan 1-on-1 dan group
Camera on dan off, microphone on dan off
Pergantian kamera depan dan belakang
Pemilihan kualitas manual dan otomatis
Bitrate, resolusi, dan frame rate adaptif
Mode bandwidth rendah
Picture-in-picture dan background call
Screen sharing
Indikator kualitas jaringan
Reconnect otomatis
Call history, missed call, durasi

Keadaan panggilan. Satu panggilan memiliki enam keadaan dan client WAJIB menampilkannya, karena panggilan yang diam tanpa keterangan membuat pengguna menutupnya lalu mencoba lagi:

Ringing
Connecting
Connected
Reconnecting
Degraded, yaitu tersambung tetapi kualitas turun sampai video dimatikan
Ended, selalu dengan sebab

Jaringan yang terputus sementara TIDAK BOLEH langsung mengakhiri panggilan. Client mencoba ICE restart dan renegotiation melalui CALL_RENEGOTIATE selama jendela reconnect, dan baru berakhir bila jendela itu habis. Perpindahan dari Wi-Fi ke seluler adalah kejadian normal di ponsel, bukan kegagalan.

Sebab Ended yang WAJIB dibedakan di UI: diakhiri oleh salah satu pihak, ditolak, tidak dijawab sampai invite kedaluwarsa, gagal tersambung, dan diakhiri karena jaringan. Empat sebab pertama adalah keputusan manusia, dua terakhir adalah kegagalan sistem, dan menyamakan semuanya menjadi "Call ended" menghilangkan informasi yang dibutuhkan pengguna.

Izin memanggil. Pengguna menentukan siapa yang boleh memanggilnya, terpisah untuk voice call, video call, dan group call:

Everyone
Friends
Contacts
Nobody

Default adalah Friends. Pengguna yang diblokir tidak dapat memanggil, dan panggilan dari akun yang tidak dikenal dapat dimatikan seluruhnya. Permintaan yang tertolak dijawab BLOCKED_BY_USER atau PRIVACY_RESTRICTED, dan pemanggil TIDAK BOLEH dapat membedakan keduanya dari nomor tunggu atau dari waktu jawaban, karena perbedaan itu membocorkan apakah dirinya diblokir.

Kontrol privasi panggilan yang WAJIB tersedia: blokir panggilan dari pengguna tertentu, matikan nada panggilan tanpa menolak, matikan video call sementara tetap menerima voice call, dan matikan panggilan dari pengguna yang tidak dikenal. Semuanya diperiksa di server sebelum callee diberi tahu, sehingga pemanggil yang tidak berhak tidak pernah membuat perangkat callee berdering.

Sebelum signaling diproses, server memverifikasi authentication, device, keanggotaan conversation, izin panggilan, status block, privacy setting, dan rate limit. TIDAK BOLEH ada anonymous call signaling. Lihat section 165.

Group call. Full mesh TIDAK BOLEH dipakai di luar panggilan dua peserta, karena beban uplink tiap peserta tumbuh sebanding dengan jumlah peserta lain dan uplink adalah sumber daya paling langka pada jaringan seluler:

Dua peserta memakai P2P langsung
Tiga peserta atau lebih WAJIB memakai SFU regional
SFU hanya menerima dan meneruskan paket terenkripsi, memilih stream, mengatur bandwidth, dan meneruskan simulcast
SFU TIDAK BOLEH memiliki akses ke plaintext media
MCU yang melakukan transcoding TIDAK BOLEH dipakai pada panggilan yang diklaim E2E, karena transcoding menuntut akses plaintext

Contoh topologi:

User A --+
User B --+
User C --+--> Regional SFU --> peserta lain
User D --+
User E --+

Batas peserta yang berlaku sebagai default produk: 32 peserta audio, dan paling banyak 8 stream video aktif sekaligus sementara peserta lain mengikuti sebagai audio dengan video yang dijeda. Angka ini adalah batas produk, bukan batas protokol, dan dapat dikonfigurasi operator per region sesuai kapasitas SFU. Melewati batas dijawab QUOTA_EXCEEDED.

Perubahan keanggotaan group call WAJIB memicu re-keying melalui CALL_KEY_UPDATE, sehingga peserta yang keluar tidak dapat membaca media setelahnya dan peserta yang baru masuk tidak dapat membaca media sebelumnya.

Kualitas adaptif. Video menyesuaikan diri terhadap jaringan tanpa memutus panggilan:

Excellent, sampai 1080p
Good, 720p
Average, 480p
Poor, 360p
Very poor, audio saja

Yang dipantau: packet loss, RTT, jitter, bandwidth tersedia, bitrate terkirim, frame yang dijatuhkan, dan kualitas audio serta video yang diterima. Ketika jaringan memburuk, urutan penurunannya adalah bitrate, lalu resolusi, lalu frame rate, lalu video dimatikan dan audio dipertahankan. Ketika jaringan membaik, kualitas dinaikkan bertahap, bukan langsung melompat, karena lompatan memicu osilasi.

Low data mode mengikuti enum BandwidthMode pada section 75. Pada LowData, audio diprioritaskan, resolusi dan frame rate diturunkan, HD dimatikan, dan frekuensi keyframe dikurangi. Pada UltraLowData, video dimatikan ketika jaringan tidak memadai dan audio tetap berjalan.

Codec. Audio memakai codec speech realtime dengan latensi rendah, bitrate rendah, dan ketahanan terhadap packet loss. Video memakai codec dengan akselerasi hardware serta dukungan luas di browser, Android, dan iOS. Pemilihan codec dilakukan lewat negotiation berdasarkan kemampuan perangkat, dan TIDAK BOLEH ada satu bitrate tetap yang dipaksakan untuk semua perangkat.

Screen sharing tersedia pada video call dengan tiga cakupan: seluruh layar, satu jendela aplikasi, dan satu tab browser. Screen sharing mengikuti model enkripsi yang sama dengan media panggilan. Indikator berbagi layar WAJIB terlihat oleh yang membagikan selama berbagi berjalan, karena berbagi layar yang terlupakan adalah kebocoran data yang paling sering terjadi dalam praktik.

TURN. Fallback dipakai hanya ketika P2P gagal, misalnya karena symmetric NAT, carrier NAT, firewall perusahaan, kebijakan jaringan, atau UDP diblokir:

TURN dideploy per region, misalnya Singapore dan Japan untuk Asia, Germany dan Netherlands untuk Eropa, serta US East dan US West
Client memilih berdasarkan latensi dan ketersediaan, dengan urutan primary, secondary, lalu region lain
Kredensial TURN bersifat sementara, diambil melalui CALL_TURN_FETCH, dan TIDAK BOLEH ditanam di dalam aplikasi
TURN adalah relay dan TIDAK BOLEH dapat membaca plaintext media

Call history menyimpan metadata minimal: call_id, pemanggil, peserta, jenis panggilan, waktu mulai, waktu berakhir, durasi, dan status. Isi panggilan TIDAK BOLEH direkam server dalam bentuk apa pun.

Contoh tampilan riwayat:

Voice call, 12 menit
Video call, 34 menit
Missed call

Rekaman panggilan bukan bagian dari Migo. Bila fitur itu pernah ditambahkan, fitur tersebut WAJIB dinyatakan tidak end-to-end, WAJIB meminta persetujuan seluruh peserta sebelum dimulai, dan WAJIB menampilkan indikator selama berjalan. Rekaman diam-diam TIDAK BOLEH ada.

Call UI. Layar panggilan masuk menampilkan avatar, nama, jenis panggilan, dan dua tombol yaitu accept dan reject. Selama panggilan tersedia mute, speaker, kamera, ganti kamera, tambah peserta, screen share, indikator kualitas jaringan, dan end call. Panggilan masuk memakai notifikasi panggilan yang sesuai aturan platform, dan pada Android dapat berupa layar penuh bila aturan versi OS mengizinkannya.

Identitas dan verifikasi. Setiap device memiliki identitas kriptografis sesuai section 9 dan section 47:

Setiap panggilan menghasilkan session key baru dari ephemeral key. Satu static key TIDAK BOLEH dipakai untuk semua panggilan
Key dirotasi per panggilan, per sesi, saat peserta berubah, dan saat terjadi security event
Pengguna dapat memverifikasi identitas lawan bicara lewat security code, safety number, atau pemindaian QR
Perubahan identity key WAJIB menampilkan peringatan keamanan, bukan diterima diam-diam
Pengguna dapat melihat daftar device miliknya, misalnya Android, Chrome, dan iPhone, lalu memverifikasi atau mencabutnya
Device yang dicabut TIDAK BOLEH dapat memakai call key dan session lama

Penilaian kualitas. Setelah panggilan berakhir, pengguna dapat memberi penilaian Excellent, Good, Average, atau Poor, dengan keterangan OPSIONAL berupa masalah audio, masalah video, masalah koneksi, atau panggilan terputus. Penilaian dipakai untuk memantau kualitas dan TIDAK BOLEH menyertakan isi panggilan.

Metrik yang dikumpulkan server: waktu setup panggilan, tingkat keberhasilan dan kegagalan, tingkat reconnect, pemakaian TURN, tingkat keberhasilan P2P, latensi rata-rata, packet loss, region, dan beban SFU. Angka kualitas dikirim lewat CALL_STATS yang berkelas Droppable dan dibatasi ukurannya di section 171. Isi panggilan, transkrip, dan sampel audio TIDAK BOLEH dikirim. Nama metrik dan labelnya ada di section 174.

Tingkat keberhasilan P2P dipantau sebagai angka produk, bukan hanya angka teknis: persentase panggilan yang berhasil P2P, yang jatuh ke TURN, dan yang gagal seluruhnya. Angka itu yang menentukan di region mana infrastruktur berikutnya dibangun, karena keputusan kapasitas yang tidak didasarkan pengukuran adalah tebakan mahal.

Perlindungan penyalahgunaan. Rate limit diterapkan pada undangan panggilan, panggilan berulang ke tujuan yang sama, panggilan gagal berturut-turut, permintaan join group, dan jumlah langganan stream di SFU. Yang dicegah adalah call spam, call bombing, bot panggilan otomatis, signaling palsu, dan penghabisan koneksi. Pelanggaran dijawab RATE_LIMITED dengan retry_after. Lihat section 120 dan section 121.

Perilaku ketika callee tidak terhubung. Server mengirim push notification yang hanya memuat call_id dan penanda bahwa ada panggilan. Plaintext pesan, isi signaling, dan data private TIDAK BOLEH masuk ke payload push. Push berfungsi sebagai wake-up, dan ringing lokal dimulai setelah client terhubung. Bila callee tidak pernah terhubung sampai invite kedaluwarsa, panggilan berakhir sebagai missed call. Lihat section 77 dan section 165.

Panggilan TIDAK BOLEH masuk ke offline queue. Panggilan bersifat realtime dan kehilangan maknanya bila dikirim terlambat; ketika perangkat offline, tombol call dinonaktifkan dengan alasan yang jelas. Aturan ini sudah ditetapkan di section 17 dan diulang di sini karena sering dilanggar oleh implementasi yang menyamakan semua frame.

Implementasi Android:

Notifikasi panggilan masuk dan kontrol pada lock screen
UI panggilan layar penuh bila aturan versi OS mengizinkannya
Bluetooth, speaker, microphone, kamera, dan proximity sensor
Background call dengan foreground execution hanya selama panggilan berlangsung
Audio focus, perubahan jaringan, dan perubahan perangkat Bluetooth ditangani sebagai kejadian normal
Permission kamera, microphone, dan notifikasi diminta saat fitur dipakai, TIDAK BOLEH saat aplikasi dibuka
Service latar belakang permanen TIDAK BOLEH berjalan ketika tidak ada panggilan

Implementasi Web:

Next.js dengan WebRTC dan getUserMedia
Kamera, microphone, screen capture, pemilihan perangkat, dan pemilihan output audio bila browser mendukungnya
Picture-in-picture
Pemantauan jaringan dan reconnect
Permission kamera dan microphone diminta saat fitur dipakai

Arsitektur signaling dan protocol WAJIB tetap kompatibel dengan client iOS native yang memakai implementasi media WebRTC, mencakup kamera, microphone, Bluetooth, background call, push notification, dan kontrol panggilan.

Tanggung jawab server Migo pada panggilan terbatas pada authentication, signaling, negosiasi ICE dalam bentuk blob tersegel, permission, metadata panggilan, push notification, koordinasi TURN, koordinasi SFU, dan metrik. Server Migo TIDAK BOLEH menangani plaintext media panggilan private.

Ringkasan model akhir:

Private chat memakai end-to-end encryption
Private voice call memakai P2P ditambah E2E
Private video call memakai P2P ditambah E2E
Group voice call memakai SFU ditambah E2E
Group video call memakai SFU ditambah E2E
Ketika P2P gagal, jalurnya menjadi TURN dan media tetap E2E
Komunikasi server ke server memakai koneksi terautentikasi dan terenkripsi sesuai section 7
Public Room dan Managed Room memakai encrypted transport dan isinya dapat dibaca server untuk moderation, sehingga keduanya bukan jalur end-to-end; kemungkinan protokol room terenkripsi berada di luar lingkup MWP/1 sesuai section 8


181. DATABASE ACCESS LAYER AND ORM

STATUS: BUILT untuk migo-store. Keputusannya ada di ADR-0012, pemetaan tabel ke entity ada di docs/04-data-model.md, dan aturan test ada di docs/10-testing-strategy.md. Bagian ini ditempatkan di akhir dokumen karena penomoran section 1 sampai 135 dibekukan, bukan karena prioritasnya rendah.

Akses database WAJIB melalui ORM SeaORM. Yang dilarang bukan SQL, melainkan SQL yang menuliskan ulang bentuk tabel. Daftar kolom, urutan kolom, dan nama kolom pada select maupun insert WAJIB berasal dari entity hasil generate, karena daftar kolom yang ditulis tangan akan berbeda dari schema pada hari seseorang menambah satu kolom dan lupa mengubah satu query. Kegagalan seperti itu tidak berbunyi saat compile dan tidak berbunyi saat test, melainkan berbunyi di produksi sebagai kolom yang tidak pernah terbaca.

Entity dihasilkan oleh tools/entity-codegen dari file di server/migrations, satu modul per tabel, dan WAJIB tidak diedit tangan. Regenerasi dijalankan dengan make entities dan kesegarannya dijaga oleh make entity-check yang menjadi bagian dari make ci. Arahnya satu, yaitu dari migration ke entity, sehingga schema tetap satu sumber kebenaran dan tidak pernah ada dua tempat yang mengklaim bentuk tabel yang sama.

Storage trait pada migo-store tetap menjadi satu-satunya API yang dilihat lapisan di atasnya. Trait berbicara dalam model domain, bukan dalam entity, sehingga tidak ada modul di luar migo-store yang boleh mengimpor entity dan tidak ada satu pun pemanggil yang perlu tahu ORM apa yang dipakai. Properti inilah yang membuat ORM dapat diganti tanpa menyentuh satu baris pun di luar crate ini, dan properti itu hilang begitu satu entity bocor ke signature publik.

SQL yang ditulis tangan DIPERBOLEHKAN hanya ketika query builder tidak dapat menyatakan bentuknya, misalnya CTE yang dirujuk lebih dari satu kali atau delete dengan using. Dalam kasus itu materialisasi hasilnya tetap melalui entity atau melalui pembacaan kolom bernama, TIDAK BOLEH melalui indeks posisi, karena projection yang bertambah satu kolom akan mulai membaca kolom yang salah tanpa satu pun pesan error. Setiap query semacam ini WAJIB menyertakan alasan mengapa builder tidak dipakai.

Yang hilang dari perpindahan ini WAJIB dicatat jujur, bukan disembunyikan. Migrator sqlx menghitung checksum setiap file yang sudah diterapkan dan menolak berjalan bila file itu berubah; migrator SeaORM hanya mencatat bahwa sebuah nama sudah dijalankan. Akibatnya file migration yang sudah diterapkan lalu diedit akan berarti sudah diterapkan pada database lama dan berarti sesuatu yang lain pada database baru, dan tidak ada gate otomatis yang dapat menangkapnya. Aturannya karena itu menjadi aturan manusia yang ditulis di kepala setiap file migration, dan perbaikan atas kesalahan WAJIB berupa file migration berikutnya, bukan penulisan ulang sejarah.

Yang juga hilang adalah advisory lock di sekeliling proses migrasi. Migrator sqlx mengambilnya sendiri sehingga dua proses migod yang start bersamaan tidak pernah menerapkan file yang sama dua kali; migrator SeaORM tidak. Lock itu karena itu diambil kembali secara eksplisit oleh migo-store sebelum migrasi dijalankan dan dilepas setelahnya, dan tanpa lock itu deploy dengan dua replika adalah balapan yang kalahnya berupa schema separuh jadi.

Batas minimum versi Rust menjadi 1.94 karena sea-orm 2.0 yang menetapkannya. Angka itu bukan pilihan proyek ini, sehingga dapat bergerak tanpa ada file di repositori ini yang diubah, dan job MSRV pada section 177 ada tepat untuk menangkap pergerakan itu di tempat yang benar.

Aturan privasi pada section 46 dan section 174 tetap ditegakkan oleh SQL yang dihasilkan, bukan oleh ingatan reviewer. Kolom kredensial push WAJIB tidak pernah ikut dalam select mana pun yang dipakai jalur pembacaan device, sehingga pembacaan itu memakai partial model yang tidak memuat kolom tersebut sama sekali. Kredensial yang tidak pernah masuk ke struct tidak dapat tercetak ke log karena kelalaian, dan itu adalah jaminan struktural, bukan konvensi.
