# Files for the demo

What `scripts/demo.py` can't make with Python's standard library. Each is
made by a command, to make it again or another like it.

- `lisbon-sunset.webm` — the sun of `lisbon-sunset.png` going down (6 s,
  VP9, 12 KB), for the video in *Lisbon 2026*:

  ```sh
  S='lt((X-240)*(X-240)+(Y-150-12*T)*(Y-150-12*T)\,3025)*lt(Y\,180)'   # the sun
  W='20*eq(mod(floor((X+24*T)/12)+floor(Y/6)\,5)\,0)*lt(abs(X-240)\,80)' # its glitter on the river
  ffmpeg -f lavfi -i "color=black:s=480x270:r=15:d=6,format=gbrp,geq=\
  r='if($S\,255\,if(lt(Y\,180)\,(255-60*Y/180)*(1-0.06*T)\,30+$W))':\
  g='if($S\,214-12*T\,if(lt(Y\,180)\,(120+40*Y/180)*(1-0.06*T)\,80+$W))':\
  b='if($S\,102-8*T\,if(lt(Y\,180)\,(90+60*Y/180)*(1-0.02*T)\,140+$W))'" \
    -c:v libvpx-vp9 -crf 42 -b:v 0 -pix_fmt yuv420p -an -row-mt 1 lisbon-sunset.webm
  ```
