FROM sysg-usecase-base

COPY tests/docker/usecase/single-project-compat/legacy.yaml /usecase/legacy.yaml
COPY tests/docker/usecase/single-project-compat/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
