# OpenWork LibreOffice App image

This image contains the official CLI-Anything LibreOffice harness pinned to
commit `810c18b0d1ab9b234bc996c9fd999318523a3ef0` and LibreOffice headless. Its
fixed entrypoint accepts one bounded JSON capability invocation on stdin and
creates only `.xlsx` and `.pptx` artifacts in `/workspace/output`.

Build locally with:

```sh
docker build -t openwork/libreoffice-app:dev containers/libreoffice-app
```

Production must publish the image and configure OpenWork with the resulting
registry digest. The App runtime disables network access and uses a read-only
root filesystem.
