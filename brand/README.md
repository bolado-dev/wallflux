# Recursos de marca

| Fichero | Qué es |
| --- | --- |
| `wallflux-logo-source.png` | El original sin tocar. Parte siempre de aquí. |
| `wallflux-mark.png` | Recortado al glifo, sin margen: el espaciado es cosa de la interfaz. |

Los dos son blanco sobre transparente, que es lo que pide el cromado oscuro de
la aplicación. Dos cosas antes de usarlos en otro sitio:

- **No pongas la marca desnuda sobre fondo claro.** Es blanca: desaparece. El
  icono de la aplicación existe justo por eso — es la marca centrada sobre un
  cuadrado redondeado oscuro, así que sobrevive a un lanzador de tema claro.
- El glifo lleva una sombra de pliegue suave (cuerpo `#F4F4F4`, pliegue
  `#C6C5C5`). Ese sombreado es parte del diseño, así que evita aplanarlo a un
  blanco plano.

Para regenerar el icono después de cambiar el arte:

```sh
magick brand/wallflux-mark.png -resize 512x gui/src/assets/wallflux-mark.png
magick -size 512x512 xc:none \
  -fill '#0a0a0a' -draw 'roundrectangle 0,0 511,511 112,112' \
  \( brand/wallflux-mark.png -resize 340x \) -gravity center -composite \
  gui/src-tauri/icons/icon.png
```

`./install.sh --with-gui` lo recoge de ahí y lo instala en el tema hicolor en
seis tamaños.
