FROM sysg-usecase-base

COPY tests/docker/usecase/port-occupied-fails/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/port-occupied-fails/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
