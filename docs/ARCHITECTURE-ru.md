# mpris-bridge: архитектура, алгоритмы и интеграция

Документ описывает актуальную реализацию из исходников `main.rs` (демон, далее — “bridged”) и `mpris-bridgec.rs` (CLI‑клиент, далее — “bridgec”).

Содержание:
- Обзор
- Компоненты и взаимодействие
- Алгоритмы (текст и визуально)
- Форматы данных и IPC API
- Конфигурация
- Интеграция с Waybar/Eww и другими UI
- Возможности для конечных пользователей
- Надёжность, производительность, безопасность
- Отладка и диагностика
- FAQ и известные особенности


## Обзор

mpris-bridge — это легковесный слой между MPRIS‑плеерами и пользовательским UI (Waybar, Eww и пр.), который:
- Автоматически выбирает “активный” плеер на основе статусов, приоритетов и фокуса окна Hyprland.
- Подписывается на события D‑Bus только для MPRIS (с узкими match‑правилами), реагируя на запуск/остановку плееров и изменения свойств.
- Поддерживает “follower”: один процесс `playerctl -F` для выбранного плеера, который стримит метаданные, позицию, обложку и т.п.
- Экспортирует состояние в атомарный снимок `state.json` и поток событий `events.jsonl`.
- Предоставляет локальный UNIX‑сокет для управления (play-pause/next/previous/seek/position).
- Содержит CLI‑клиент `mpris-bridgec` для управления и для удобного “watch” вывода меток.


## Компоненты и взаимодействие

- Демон `mpris-bridged`:
  - Подписывается на D‑Bus сигналы (узкие фильтры для MPRIS).
  - Слушает Hyprland (`hyprctl -i events`) для получения фокуса приложения → hint.
  - Ведёт множество плееров и их статусы.
  - Вычисляет и поддерживает “выбранного” плеера.
  - Сопровождает одного follower’a (`playerctl -F`) по выбранному плееру.
  - Пишет JSON‑состояние и события.
  - IPC: локальный UNIX‑сокет для команд управления.

- CLI `mpris-bridgec`:
  - Отправляет JSON‑команды по UNIX‑сокету (с fallback’ом на прямой `playerctl`, если сокет недоступен).
  - Режим watch: читает `state.json` (начальный снимок) и “хвостит” `events.jsonl`, печатая с форматированием и обрезкой/экранированием.

- Файлы и сокет (по умолчанию под `$XDG_RUNTIME_DIR/mpris-bridge/`):
  - `state.json` — последняя полная проекция состояния (UiState).
  - `events.jsonl` — поток состояний (по строке JSON на событие).
  - `mpris-bridge.sock` — IPC‑сокет (права 0600).


## Алгоритмы

### 1) Выбор активного плеера (selection)

Пусть:
- known players: множество известных имён (из `playerctl -l`), отфильтрованных include/exclude.
- status map: карта “имя → статус (Playing/Paused/Stopped)”.
- focus hint: подсказка из активного окна (firefox/spotify/vlc/mpv), если её можно вывести из класса окна.

Шаги:
1. Собрать `players` = known ∩ include/exclude.
2. Если `players` пуст, вернуть None.
3. `playing` = подмножество `players` со статусом “Playing”.
4. Если есть `playing`:
   - если есть `focus hint`, и среди `playing` есть имя, начинающееся с `hint` → выбрать его;
   - иначе пройти по `priority` (из конфигурации) и выбрать первое совпадение по префиксу;
   - иначе взять первый из `playing`.
5. Если нет `playing`:
   - если `remember_last` и `last_selected` всё ещё присутствует в `players` → выбрать его;
   - иначе если есть `focus hint` → выбрать любое имя из `players`, начинающееся с `hint`;
   - иначе пройти по `priority` → выбрать первое совпадение;
   - иначе, если `fallback == "any"` → взять первый `players[0]`; иначе вернуть None.

При смене выбора:
- Немедленно отправить “быстрый снапшот” (`emit_quick_snapshot`) для визуально мгновенного обновления.
- Фолловер перезапускается на выбранного плеера.

Визуально (упрощённый блок‑диаграм):

```
[Known players + statuses + focus hint]
            |
            v
  [Есть Playing среди них?] --нет--> [remember_last?] --да--> [last still present?] --да--> select last
          |                             |                               |                         |
         да                             нет                             нет                       |
          v                              v                               v                        v
 [focus hint среди Playing?]    [focus hint среди players?]    [priority среди players?]   [fallback == any?]
          |                                |                              |                        |
         да                                да                            да                        да
          v                                v                              v                        v
      select by hint                 select by hint               select by priority             select first
          |                                |                              |                        |
         нет                               нет                           нет                       нет
          v                                v                              v                        v
 [priority среди Playing?]        [fallback == any?]                  return None               return None
          |
         да/нет
          v
  select by priority / first
```

### 2) Подписка на D‑Bus и предотвращение роста памяти

- Применены узкие фильтры (add_match):
  - NameOwnerChanged только для имён `org.mpris.MediaPlayer2.*` (`arg0namespace`).
  - PropertiesChanged только на пути `/org/mpris/MediaPlayer2` и только для интерфейсов `org.mpris.MediaPlayer2.Player` и `org.mpris.MediaPlayer2` (`arg0` + `path`).

Это уменьшает поток сигналов, которые вообще доходят до процесса (и не буферизуются в очереди брокера), устраняя “раздувание” памяти dbus‑broker.

- Дебаунс тяжёлых операций:
  - `seed_players()` (список плееров) — не чаще, чем раз в ~300 мс.
  - `refresh_statuses()` (массовый опрос статусов) — не чаще, чем раз в ~250 мс.
  - Тяжёлые операции переносятся в фоновые задачи (`task::spawn`), чтобы основной цикл чтения D‑Bus оставался быстрым.

Визуально (последовательность при D‑Bus сигнале):

```
DBus broker -> bridged (MessageStream)
     |            |
     |  [Header match: iface/member/path/arg0?] --нет--> drop early
     |            |
    да            v
                  [Update timer/debounce gate]
                  |         \
                 pass      hold (skip now)
                  |
               task::spawn(async {
                  seed/refresh + recompute_selected + quick snapshot
               })
```

### 3) “Follower” по выбранному плееру

- Запускается `playerctl -p <name> metadata ... -F`, который стримит строки в формате:
  `{{status}}|{{playerName}}|{{title}}|{{artist}}|{{mpris:length}}|{{mpris:artUrl}}|{{position}}|{{xesam:url}}`
- На каждую строку:
  - Обновляется карта статусов для выбранного имени.
  - При существенных изменениях (status/title/artist/url) — читаются возможности (CanGoNext/Previous) через `busctl get-property`, с локальной оптимизацией и политикой для YouTube в Firefox (без плейлиста: next=1, prev=0).
  - Формируется `UiState`, обрезаются `title/artist` по лимитам, считается `position_str/length_str`.
  - Обложка:
    - `file://` → копия/ссылка в `current_cover`;
    - `http(s)://` → кэширование по SHA‑1 в `cache_dir` + копия/ссылка;
    - иначе — `default_cover`.
  - Запись состояния: атомарно в `state.json` и append в `events.jsonl`.

Визуально:

```
[playerctl -F] ---> [bridged follower task]
         |                 |
         |          parse line (8 fields)
         |                 |
         |        update status[name]
         |                 |
         |      get caps (debounced by content change)
         |                 |
         |        build UiState + cover
         |                 |
         |       write_state(state.json + events.jsonl)
```

### 4) IPC сервер

- UNIX‑сокет `mpris-bridge.sock` (0600), синхронный обработчик на отдельном блокирующем пуле:
  - Читает строки JSON с полем `cmd` и опциональным `player`.
  - Разрешает имя плеера: явный `player` или текущий выбранный.
  - Выполняет через `playerctl` одну из команд:
    - `play-pause`, `next`, `previous`
    - `seek {offset}` → `playerctl position "N+" / "N-"`
    - `set-position {position}` → `playerctl position "N"`
  - Отвечает `{"ok":true}\n` или `{"ok":false}\n`.

- CLI `bridgec` отправляет тот же JSON и ожидает одну строку ответа.

Визуально:

```
client (bridgec/UI) --json--> [UNIX socket server] --runs--> playerctl ...
                                 |                           ^
                                 v                           |
                            {"ok":true/false}  <------------
```


## Форматы данных и IPC API

### UiState (и строки в events.jsonl)
Пример структуры (json, camelCase):
```json
{
  "name": "spotify",
  "title": "Song Title",
  "artist": "Artist",
  "status": "Playing",
  "position": 42.1,
  "positionStr": "0:42",
  "length": 210.0,
  "lengthStr": "3:30",
  "thumbnail": "/home/user/.config/eww/image.jpg",
  "canNext": 1,
  "canPrev": 1
}
```

- `state.json` всегда содержит последний снимок (перезапись атомарно через временный файл).
- `events.jsonl` — каждая строка: `UiState` в виде JSON.

### IPC команды (JSON по сокету)
- Play/pause:
  ```json
  {"cmd":"play-pause","player":"optional_name"}
  ```
- Next/previous:
  ```json
  {"cmd":"next","player":"optional_name"}
  {"cmd":"previous","player":"optional_name"}
  ```
- Seek (смещение в секундах ±):
  ```json
  {"cmd":"seek","offset":5.0,"player":"optional_name"}
  {"cmd":"seek","offset":-10.0}
  ```
- Set absolute position:
  ```json
  {"cmd":"set-position","position":120.0}
  ```

Ответ: `{"ok":true}\n` или `{"ok":false}\n`.


## Конфигурация

Читается `~/.config/mpris-bridge/config.toml`. Параметры:

- `selection`:
  - `priority: [ "firefox", "spotify", "vlc", "mpv" ]`
  - `remember_last: true`
  - `fallback: "any" | "none"`
  - `include: [ ]` — разрешённые префиксы имён MPRIS плееров
  - `exclude: [ ]` — исключённые префиксы
- `art`:
  - `enabled: true`
  - `download_http: true`
  - `timeout_ms: 5000`
  - `cache_dir`, `default_image`, `current_path`, `use_symlink`
- `output`:
  - `snapshot_path` (по умолчанию `$XDG_RUNTIME_DIR/mpris-bridge/state.json`)
  - `events_path` (по умолчанию `$XDG_RUNTIME_DIR/mpris-bridge/events.jsonl`)
  - `pretty_snapshot: false`
- `presentation`:
  - `truncate_title: 120`
  - `truncate_artist: 120`
- `logging`:
  - `level: "warn"` (зарезервировано на будущее)

Примечание: В коде есть `expand()` для подстановки `$HOME`, `$XDG_RUNTIME_DIR` и др.


## Интеграция с Waybar/Eww/др. UI

Есть два способа потребления данных:

1) Через `mpris-bridgec watch`:
   - Выводит одну строку при старте (по `state.json`) и затем обновления при новых событиях (по `events.jsonl`).
   - Опции:
     - `--format` — шаблон, например: `"{artist}{sep}{title}"`
     - `--truncate N` — обрезка длины (с `…`)
     - `--pango-escape` — экранирование `& < > ' "` для безопасного вывода в Pango/markup.

   Пример для Waybar custom module:
   ```json
   {
     "custom/mpris": {
       "exec": "mpris-bridgec watch --format \"{artist}{sep}{title}\" --pango-escape",
       "return-type": "json"
     }
   }
   ```
   Или без JSON: просто строка в stdout подойдёт для текстового поля.

2) Напрямую читать `state.json`/`events.jsonl`:
   - Ваш скрипт/виджет может читать последнюю строку из `events.jsonl` или периодически перечитывать `state.json`.
   - `events.jsonl` удобен для реактивных обновлений без таймеров.

Для кнопок управления:
- Вызывать `mpris-bridgec play-pause`, `next`, `previous`, `seek +5`, `set-position 60` и т.д.


## Возможности для конечных пользователей

- Авто‑выбор активного плеера: умная логика на основе Playing/фокуса/приоритетов/last‑known.
- Мгновенные обновления заголовка/артиста/позиции текущего плеера.
- Обложки: локальные пути, HTTP с кэшированием, дефолтная картинка.
- Возможности навигации: `canNext/canPrev` (с политикой для YouTube без плейлиста).
- Управление воспроизведением через CLI или IPC из любого UI.
- Форматируемые подписи для панелей/виджетов.
- Стабильная работа без “раздувания” памяти dbus‑broker (узкие match‑правила + дебаунс).


## Надёжность, производительность, безопасность

- D‑Bus:
  - Узкие `add_match` исключают “шумные” сигналы → меньше очередей → стабильное потребление памяти брокером.
  - Основной цикл быстро читает поток; тяжёлые действия — в фоновых задачах.

- Watchdog follower’a:
  - Каждые 2 сек проверяется флаг `follower_alive`. При сбое — перезапуск.

- Файловый вывод:
  - `state.json` пишется атомарно через временный файл.
  - `events.jsonl` — просто append; при желании можно добавить ротацию.

- IPC сокет:
  - Создаётся с правами 0600, только для текущего пользователя.

- Безопасность:
  - Нет `unsafe` Rust.
  - Внешние утилиты (`playerctl`, `busctl`, `hyprctl`) вызываются с подавлением stdout/stderr, где уместно.


## Отладка и диагностика

Полезные команды:
- Мониторинг брокера:
  - `systemctl --user status dbus-broker.service`
  - `watch -n1 "cat /proc/$(systemctl --user show -p MainPID --value dbus-broker.service)/status | egrep 'VmRSS|VmSize'"`
- Поток MPRIS‑сигналов:
  - `busctl --user monitor "type='signal',interface='org.freedesktop.DBus.Properties',path='/org/mpris/MediaPlayer2'"`
  - `busctl --user monitor "type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='org.mpris.MediaPlayer2'"`
- Список плееров и статусы:
  - `playerctl -l`
  - `playerctl -p <name> status`
- Логи bridged:
  - Запуск в терминале — видеть `eprintln!` ошибки.
- Проверка IPC:
  - `printf '{"cmd":"play-pause"}\n' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/mpris-bridge/mpris-bridge.sock`


## FAQ и особенности

- “Почему обновления такие быстрые?”  
  Потому что метаданные выбранного плеера идут из `playerctl -F` (стрим), без поллинга и без дебаунса.

- “Зачем дебаунс на D‑Bus?”  
  Чтобы не запускать тяжёлые `playerctl -l` и массовые `status` слишком часто, и чтобы broker/клиент не копил очереди событий.

- “Можно ещё ускорить переключение между плеерами?”  
  В большинстве случаев оно и так моментально. Если нужно — можно уменьшить дебаунс (например, 150–200 мс), либо вынести задержки в конфиг.

- “Что если плеер не вполне стандартный MPRIS?”  
  Почти все используют `path='/org/mpris/MediaPlayer2'` и интерфейсы `org.mpris.MediaPlayer2(.Player)`. Если встретится экзотика — follower всё равно обеспечит актуальность выбранного плеера, а NameOwnerChanged (по префиксу) увидит появление/исчезновение.


---