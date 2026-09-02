# Tests de Meeting Assistant v4.2

Ejecuta `Run tests.bat`.

La suite prueba sin hardware real:

- preferencia del dispositivo configurado;
- fallback al dispositivo predeterminado;
- prioridad del micrófono interno;
- Plantronics -> Realtek;
- ausencia temporal de dispositivos;
- política estable: tras un fallback no vuelve a cambiar mientras el nuevo
  dispositivo siga disponible;
- conversión 44.1 kHz -> 48 kHz;
- mezcla de loopback estéreo a mono;
- inserción de silencio para preservar la línea temporal;
- integración de los bucles de recuperación hot-plug;
- sintaxis del programa.

La app ahora implementa hot-plug real durante la grabación. Si el dispositivo
activo desaparece, mantiene la reunión viva, busca otro dispositivo y rellena
el intervalo del cambio con silencio. Si no hay dispositivo disponible,
continúa esperando hasta que aparezca uno o el usuario finalice la reunión.

Sigue siendo recomendable un único smoke test físico en Windows porque el
comportamiento exacto de desconexión depende de los drivers.


## v4.3

La transcripción muestra ahora un porcentaje global de 0 a 100.
El porcentaje está ponderado por la duración real de las dos pistas, de modo
que no se asume que micrófono y audio del PC duren exactamente lo mismo.
La lógica del porcentaje está cubierta por tests independientes.


## v4.4.7

- Todos los botones, switches y menús muestran cursor de mano.
- El hover de texto/iconos se enlaza también a los elementos internos de
  CustomTkinter para que todo el botón responda igual.
- El botón de micrófono mantiene hover visual incluso fuera de una grabación;
  la acción de mute sigue teniendo efecto únicamente durante una grabación.
- El icono de aplicación ha sido sustituido por el nuevo `.ico`.
- El entrypoint principal pasa de `meeting.py` a `app.py`.
